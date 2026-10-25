use crate::managed::{Scope, Target};
use crate::protocol::Status;
use serde_json::{json, Value};

// both explicit entrypoints share the renderer; managed admission must never
// fall back to the legacy transport's permissive request handling.
pub fn managed_status(scope: &Scope, context: &Value) -> Status {
    if scope.target != Target::Application {
        return Status::empty(Some(1000));
    }
    status(&scope.slot, context)
}

pub fn status(scope: &str, _: &Value) -> Status {
    if scope != "app_footer" {
        return Status::empty(Some(1000));
    }
    Status {
        refresh_ms: Some(1000),
        fragments: vec![json!({
            "text":chrono::Local::now().format("%H:%M:%S").to_string(),
            "tooltip":"Local time", "priority":5
        })],
    }
}
