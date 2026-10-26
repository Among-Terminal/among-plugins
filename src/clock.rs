//! modified after extraction from Among Terminal: typed managed-only target admission.
//! provenance and retained upstream attribution are recorded in PROVENANCE.md.

use crate::managed::{Scope, Target};
use crate::protocol::Status;
use serde_json::{json, Value};

pub fn managed_status(scope: &Scope, _: &Value) -> Status {
    if scope.target != Target::Application || scope.slot != "app_footer" {
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
