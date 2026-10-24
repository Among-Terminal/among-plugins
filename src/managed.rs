//! plugin-side API 1.2 admission. legacy processes keep their separate protocol;
//! merely extracting their sources must not silently change their identity.

use crate::protocol::Status;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::io::{self, BufRead, Write};
use std::process::ExitCode;

mod wire;

pub const MAX_FRAME_BYTES: usize = 64 * 1024;
const STATUS_FEATURES: &[&str] = &["status", "context.terminals", "context.cwd"];

#[derive(Debug)]
pub struct Profile<'a> {
    pub identity: &'a str,
    pub required: &'a [&'a str],
    pub optional: &'a [&'a str],
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scope {
    pub slot: String,
    pub target: Target,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
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
    if terminals.len() > 32
        || serde_json::to_vec(terminals).map_or(true, |bytes| bytes.len() > 28 * 1024)
    {
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
                && !cwd.as_str().is_some_and(|text| {
                    text.len() <= 4096
                        && serde_json::to_vec(cwd).is_ok_and(|bytes| bytes.len() <= 8192)
                })
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
    mut input: impl BufRead,
    mut output: impl Write,
    mut status: impl FnMut(&Scope, &Value) -> Status,
) -> io::Result<()> {
    if !profile.valid() {
        return Err(wire::invalid());
    }
    let mut admission: Option<Admission> = None;
    while let Some(bytes) = wire::read_frame(&mut input)? {
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
                admission.last_request = Some(request_id);
                let result = status(&scope, &context);
                wire::write_frame(
                    &mut output,
                    &json!({"type":"status","session":admission.session,
                    "generation":admission.generation,"request_id":request_id,
                    "refresh_ms":result.refresh_ms,"fragments":result.fragments}),
                )?;
            }
        }
    }
    Ok(())
}

pub fn run(profile: &Profile<'_>, status: impl FnMut(&Scope, &Value) -> Status) -> ExitCode {
    match serve(profile, io::stdin().lock(), io::stdout().lock(), status) {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}
