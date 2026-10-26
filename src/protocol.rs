//! modified after extraction from Among Terminal: shared status data only;
//! the permissive legacy runner and numeric scope adapter were removed.
//! see PROVENANCE.md for the unchanged historical extraction input.

use serde::Serialize;
use serde_json::Value;

#[derive(Serialize)]
pub struct Status {
    pub refresh_ms: Option<u64>,
    pub fragments: Vec<Value>,
}

impl Status {
    pub fn empty(refresh_ms: Option<u64>) -> Self {
        Self {
            refresh_ms,
            fragments: Vec::new(),
        }
    }
}

pub fn terminals(context: &Value) -> impl Iterator<Item = &Value> {
    context
        .get("terminals")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
}
