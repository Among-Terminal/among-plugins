use std::io::{self, BufRead, Write};
use std::process::ExitCode;

use serde::Serialize;
use serde_json::{json, Value};

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

/// Strict decimal scope IDs keep malformed requests from targeting other panes.
pub fn scope_id(scope: &str, prefix: &str) -> Option<u64> {
    let id = scope.strip_prefix(prefix)?;
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    id.parse().ok()
}

pub fn terminals(context: &Value) -> impl Iterator<Item = &Value> {
    context
        .get("terminals")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
}

/// Exit on IO failure: the host discards stderr, but observes process death.
pub fn run(mut status: impl FnMut(&str, &Value) -> Status) -> ExitCode {
    let input = io::stdin();
    let output = io::stdout();
    let mut output = output.lock();
    for line in input.lock().lines() {
        let line = match line {
            Ok(line) => line,
            Err(_) => return ExitCode::FAILURE,
        };
        let message: Value = match serde_json::from_str(&line) {
            Ok(m) => m,
            Err(_) => continue,
        };
        let reply = match message.get("type").and_then(Value::as_str) {
            Some("hello") => json!({"type":"hello", "major":1, "minor":0}),
            Some("shutdown") => break,
            Some("status") => {
                let (Some(id), Some(scope)) = (
                    message.get("request_id").and_then(Value::as_u64),
                    message.get("scope").and_then(Value::as_str),
                ) else {
                    continue;
                };
                let result = status(scope, &message["context"]);
                json!({"type":"status", "request_id":id, "refresh_ms":result.refresh_ms, "fragments":result.fragments})
            }
            _ => continue,
        };
        if serde_json::to_writer(&mut output, &reply).is_err()
            || output.write_all(b"\n").is_err()
            || output.flush().is_err()
        {
            return ExitCode::FAILURE;
        }
    }
    ExitCode::SUCCESS
}
