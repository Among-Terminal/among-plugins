use crate::protocol::Status;
use serde_json::{json, Value};

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
