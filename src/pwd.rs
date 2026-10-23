use crate::protocol::{scope_id, terminals, Status};
use serde_json::{json, Value};

pub fn status(scope: &str, context: &Value) -> Status {
    let mut result = Status::empty(Some(1000));
    let Some(pane) = scope_id(scope, "pane_footer:") else {
        return result;
    };
    let Some(terminal) = terminals(context).find(|t| t["pane"].as_u64() == Some(pane)) else {
        return result;
    };
    let Some(path) = terminal["cwd"].as_str().filter(|s| !s.is_empty()) else {
        return result;
    };
    let text = path.chars().take(480).collect::<String>();
    result
        .fragments
        .push(json!({"text":text,"tooltip":path,"priority":10}));
    result
}
