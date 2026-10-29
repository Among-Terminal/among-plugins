//! Plugin-side API 1.2 admission and bounded, identity-scoped stdio dispatch.

use crate::protocol::Status;
use serde::{Deserialize, Serialize, Serializer};
use serde_json::{json, Value};
use std::cell::Cell;
use std::collections::BTreeSet;
use std::io::{self, BufRead, Write};
use std::process::ExitCode;
use std::rc::Rc;
use std::time::{Duration, Instant};

mod paging;
mod stdio;
mod wire;

pub const MAX_FRAME_BYTES: usize = 64 * 1024;
pub const MAX_CONTEXT_PAGE_BYTES: usize = 28 * 1024;
pub const MAX_PENDING_STATUS: usize = 15;
pub const MAX_RETAINED_ENCODED_BYTES: usize = MAX_PENDING_STATUS * MAX_FRAME_BYTES;
pub const TRAVERSAL_BUDGET: Duration = Duration::from_secs(4);
const STATUS_FEATURES: &[&str] = &[
    "status",
    "context.terminals",
    "context.cwd",
    "system.metrics",
];

#[derive(Debug)]
pub struct Profile<'a> {
    pub identity: &'a str,
    pub required: &'a [&'a str],
    pub optional: &'a [&'a str],
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Scope {
    pub slot: String,
    pub target: Target,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Target {
    Application,
    Workspace { reference: Reference },
    Pane { reference: Reference },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reference(u64);

impl Reference {
    pub fn get(self) -> u64 {
        self.0
    }
}

impl Serialize for Reference {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(&self.0)
    }
}

pub enum PageResult<S> {
    Complete(Status),
    Continue(S),
}

/// copy-only, static reducers cannot retain an input page through owned strings
/// or vectors. physical and encoded state are bounded before queue admission.
pub trait PageConsumer {
    type State: Copy + Serialize + 'static;
    fn status(&mut self, scope: &Scope, context: &Value) -> io::Result<PageResult<Self::State>>;
    fn page(
        &mut self,
        scope: &Scope,
        state: Self::State,
        context: &Value,
    ) -> io::Result<PageResult<Self::State>>;
}

struct Immediate<F>(F);

impl<F: FnMut(&Scope, &Value) -> Status> PageConsumer for Immediate<F> {
    type State = ();
    fn status(&mut self, scope: &Scope, context: &Value) -> io::Result<PageResult<()>> {
        Ok(PageResult::Complete(self.0(scope, context)))
    }
    fn page(&mut self, _: &Scope, _: (), _: &Value) -> io::Result<PageResult<()>> {
        Err(wire::invalid())
    }
}

impl<'de> Deserialize<'de> for Reference {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse::<u64>()
            .ok()
            .filter(|number| number.to_string() == text)
            .map(Self)
            .ok_or_else(|| serde::de::Error::custom("noncanonical reference"))
    }
}

impl Scope {
    fn valid(&self, features: &BTreeSet<String>) -> bool {
        match self.target {
            Target::Application => matches!(self.slot.as_str(), "app_header" | "app_footer"),
            Target::Workspace { .. } => {
                features.contains("context.terminals")
                    && matches!(self.slot.as_str(), "workspace_header" | "workspace_summary")
            }
            Target::Pane { .. } => {
                features.contains("context.terminals")
                    && matches!(self.slot.as_str(), "pane_header" | "pane_footer")
            }
        }
    }
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Request {
    ManagedHello {
        major: u32,
        minor: u32,
        identity: String,
        session: String,
        generation: String,
        features: Vec<String>,
    },
    Status {
        session: String,
        generation: String,
        request_id: u64,
        scope: Scope,
        context: Value,
    },
    ContextPage {
        session: String,
        generation: String,
        request_id: u64,
        scope: Scope,
        context: Value,
    },
    Shutdown,
}

struct Admission {
    session: String,
    generation: String,
    features: BTreeSet<String>,
    last_request: Option<u64>,
}

fn identifier(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}

fn dependencies(features: &BTreeSet<String>) -> bool {
    !features.contains("context.cwd") || features.contains("context.terminals")
}

impl Profile<'_> {
    fn valid(&self) -> bool {
        let features: BTreeSet<_> = self.required.iter().chain(self.optional).copied().collect();
        identifier(self.identity)
            && self.required.contains(&"status")
            && features.len() == self.required.len() + self.optional.len()
            && features
                .iter()
                .all(|feature| STATUS_FEATURES.contains(feature))
            && (!self.required.contains(&"context.cwd")
                || self.required.contains(&"context.terminals"))
            && (!features.contains("context.cwd") || features.contains("context.terminals"))
    }

    fn select(&self, offered: &[String]) -> io::Result<BTreeSet<String>> {
        let offered_set: BTreeSet<_> = offered.iter().collect();
        if offered.len() > 32
            || offered_set.len() != offered.len()
            || offered.iter().any(|feature| !identifier(feature))
            || self
                .required
                .iter()
                .any(|feature| !offered.iter().any(|offered| offered == feature))
        {
            return Err(wire::invalid());
        }
        let selected: BTreeSet<String> = self
            .required
            .iter()
            .chain(self.optional)
            .filter(|feature| offered.iter().any(|offered| offered.as_str() == **feature))
            .map(|feature| (*feature).to_string())
            .collect();
        if !dependencies(&selected) {
            return Err(wire::invalid());
        }
        Ok(selected)
    }
}

// check host-mediated context before invoking a consumer. a malformed scope or
// sibling field must not be accidentally interpreted as a legacy pane index.
fn context_valid(context: &Value, scope: &Scope, admission: &Admission) -> bool {
    if !context.as_object().is_some_and(|object| {
        object.keys().all(|key| {
            matches!(
                key.as_str(),
                "session" | "terminals" | "next_cursor" | "active_workspace" | "focused_pane"
            )
        })
    }) {
        return false;
    }
    if context.get("session").and_then(Value::as_str) != Some(&admission.session) {
        return false;
    }
    if !admission.features.contains("context.terminals") {
        return context.as_object().is_some_and(|object| object.len() == 1);
    }
    if scope.target != Target::Application
        && (context.get("active_workspace").is_some() || context.get("focused_pane").is_some())
    {
        return false;
    }
    let Some(terminals) = context.get("terminals").and_then(Value::as_array) else {
        return false;
    };
    if terminals.len() > 32 || wire::measure(terminals, MAX_CONTEXT_PAGE_BYTES).is_err() {
        return false;
    }
    for field in ["next_cursor", "active_workspace", "focused_pane"] {
        if let Some(value) = context.get(field).filter(|value| !value.is_null()) {
            if serde_json::from_value::<Reference>(value.clone()).is_err() {
                return false;
            }
        }
    }
    let mut panes = BTreeSet::new();
    terminals.iter().all(|terminal| {
        let Some(object) = terminal.as_object() else {
            return false;
        };
        let Some(pane) = object
            .get("pane")
            .and_then(|value| serde_json::from_value::<Reference>(value.clone()).ok())
        else {
            return false;
        };
        if !panes.insert(pane.get()) {
            return false;
        }
        let workspace = match object.get("workspace") {
            Some(value) => match serde_json::from_value::<Reference>(value.clone()) {
                Ok(reference) => Some(reference),
                Err(_) => return false,
            },
            None => None,
        };
        if match scope.target {
            Target::Application => false,
            Target::Pane { reference } => pane != reference,
            Target::Workspace { reference } => workspace != Some(reference),
        } {
            return false;
        }
        if object
            .keys()
            .any(|key| !matches!(key.as_str(), "pane" | "workspace" | "cwd" | "unavailable"))
        {
            return false;
        }
        if let Some(cwd) = object.get("cwd") {
            if !admission.features.contains("context.cwd") {
                return false;
            }
            if !cwd.is_null()
                && !cwd
                    .as_str()
                    .is_some_and(|text| text.len() <= 4096 && wire::measure(cwd, 8192).is_ok())
            {
                return false;
            }
        }
        object.get("unavailable").is_none_or(|value| {
            value.as_array().is_some_and(|fields| {
                admission.features.contains("context.cwd")
                    && fields.as_slice() == [json!("cwd")]
                    && object.get("cwd") == Some(&Value::Null)
            })
        })
    })
}

/// no output precedes admission; every reply uses the accepted binding rather
/// than copying identity/session authority from the latest arbitrary message.
pub fn serve(
    profile: &Profile<'_>,
    input: impl BufRead,
    output: impl Write,
    status: impl FnMut(&Scope, &Value) -> Status,
) -> io::Result<()> {
    serve_engine(
        profile,
        input,
        output,
        Immediate(status),
        Rc::new(Cell::new(None)),
    )
}

fn serve_engine(
    profile: &Profile<'_>,
    mut input: impl BufRead,
    mut output: impl Write,
    mut consumer: impl PageConsumer,
    deadline: stdio::Deadline,
) -> io::Result<()> {
    if !profile.valid() {
        return Err(wire::invalid());
    }
    let mut admission: Option<Admission> = None;
    let mut paging = paging::Paging::new(deadline.clone())?;
    while let Some(bytes) = wire::read_frame(&mut input)? {
        stdio::check(&deadline)?;
        let value = wire::parse(&bytes)?;
        let request: Request = serde_json::from_value(value).map_err(|_| wire::invalid())?;
        match request {
            Request::Shutdown => return Ok(()),
            Request::ManagedHello {
                major,
                minor,
                identity,
                session,
                generation,
                features,
            } => {
                if admission.is_some()
                    || major != 1
                    || minor != 2
                    || identity != profile.identity
                    || !identifier(&session)
                    || !identifier(&generation)
                {
                    return Err(wire::invalid());
                }
                let features = profile.select(&features)?;
                wire::write_frame(
                    &mut output,
                    &json!({"type":"managed_hello","major":1,"minor":2,
                    "identity":profile.identity,"session":session,"generation":generation,"features":features}),
                )?;
                admission = Some(Admission {
                    session,
                    generation,
                    features,
                    last_request: None,
                });
            }
            Request::Status {
                session,
                generation,
                request_id,
                scope,
                context,
            } => {
                let Some(admission) = admission.as_mut() else {
                    return Err(wire::invalid());
                };
                if session != admission.session || generation != admission.generation {
                    continue;
                }
                if admission
                    .last_request
                    .is_some_and(|last| request_id <= last)
                {
                    continue;
                }
                if !scope.valid(&admission.features) || !context_valid(&context, &scope, admission)
                {
                    return Err(wire::invalid());
                }
                paging.admit(&scope)?;
                let started = Instant::now();
                admission.last_request = Some(request_id);
                match consumer.status(&scope, &context)? {
                    PageResult::Complete(result) => {
                        paging::reply(&mut output, admission, request_id, result)?
                    }
                    PageResult::Continue(state) => {
                        paging.begin(admission, request_id, scope, &context, state, started)?;
                        paging.enumerate(&mut output, admission)?;
                    }
                }
            }
            Request::ContextPage {
                session,
                generation,
                request_id,
                scope,
                context,
            } => {
                let Some(admission) = admission.as_ref() else {
                    return Err(wire::invalid());
                };
                if session != admission.session
                    || generation != admission.generation
                    || !scope.valid(&admission.features)
                    || !context_valid(&context, &scope, admission)
                {
                    return Err(wire::invalid());
                }
                paging.page(
                    &mut output,
                    admission,
                    request_id,
                    &scope,
                    &context,
                    &mut consumer,
                )?;
            }
        }
    }
    if paging.is_empty() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "managed traversal interrupted",
        ))
    }
}

pub fn run(profile: &Profile<'_>, status: impl FnMut(&Scope, &Value) -> Status) -> ExitCode {
    run_paged(profile, Immediate(status))
}

pub fn run_paged(profile: &Profile<'_>, consumer: impl PageConsumer) -> ExitCode {
    let deadline = Rc::new(Cell::new(None));
    let result = (|| {
        let input = stdio::Input::new(deadline.clone())?;
        let output = stdio::Output::new(deadline.clone())?;
        serve_engine(profile, input, output, consumer, deadline)
    })();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            stdio::diagnose(error.kind());
            ExitCode::FAILURE
        }
    }
}
