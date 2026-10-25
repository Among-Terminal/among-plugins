//! protocol-only SDK reducer fixture; not an official plugin or installable asset.

use among_example_plugins::{
    managed::{self, PageConsumer, PageResult, Scope, Target},
    protocol::Status,
    pwd,
};
use serde::ser::SerializeSeq;
use serde::{Serialize, Serializer};
use serde_json::{json, Value};
use std::io;

struct Count;

fn reduce(scope: &Scope, context: &Value, previous: u64) -> io::Result<PageResult<u64>> {
    if matches!(scope.target, Target::Pane { .. }) {
        return Ok(PageResult::Complete(pwd::managed_status(scope, context)));
    }
    let count = context["terminals"]
        .as_array()
        .ok_or_else(|| io::Error::other("missing page"))?
        .len() as u64;
    let count = previous
        .checked_add(count)
        .ok_or_else(|| io::Error::other("reducer exhausted"))?;
    if context
        .get("next_cursor")
        .is_some_and(|cursor| !cursor.is_null())
    {
        Ok(PageResult::Continue(count))
    } else {
        Ok(PageResult::Complete(Status {
            refresh_ms: Some(1000),
            fragments: vec![json!({
                "text":count.to_string(),"tooltip":"Observed current-world terminal records; not a frozen snapshot","priority":10
            })],
        }))
    }
}

impl PageConsumer for Count {
    type State = u64;
    fn status(&mut self, scope: &Scope, context: &Value) -> io::Result<PageResult<u64>> {
        reduce(scope, context, 0)
    }
    fn page(&mut self, scope: &Scope, state: u64, context: &Value) -> io::Result<PageResult<u64>> {
        reduce(scope, context, state)
    }
}

#[derive(Clone, Copy)]
struct EncodedOverflow;
impl Serialize for EncodedOverflow {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut sequence = serializer.serialize_seq(Some(2048))?;
        for _ in 0..2048 {
            sequence.serialize_element(&u64::MAX)?;
        }
        sequence.end()
    }
}

struct TooMuchEncodedState;
impl PageConsumer for TooMuchEncodedState {
    type State = EncodedOverflow;
    fn status(&mut self, _: &Scope, _: &Value) -> io::Result<PageResult<Self::State>> {
        Ok(PageResult::Continue(EncodedOverflow))
    }
    fn page(
        &mut self,
        _: &Scope,
        _: Self::State,
        _: &Value,
    ) -> io::Result<PageResult<Self::State>> {
        Ok(PageResult::Continue(EncodedOverflow))
    }
}

fn main() -> std::process::ExitCode {
    let profile = managed::Profile {
        identity: "github:1431055733/sdk-pages-fixture",
        required: &["status", "context.terminals", "context.cwd"],
        optional: &[],
    };
    match std::env::args().nth(1).as_deref() {
        None => managed::run_paged(&profile, Count),
        Some("--oversize-state") => managed::run_paged(&profile, TooMuchEncodedState),
        _ => std::process::ExitCode::FAILURE,
    }
}
