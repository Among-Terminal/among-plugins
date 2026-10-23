//! protocol test fixture, not an official status plugin or installable package.
use among_example_plugins::{managed, protocol::Status};
use serde_json::json;
use std::process::ExitCode;

fn main() -> ExitCode {
    let profile = managed::Profile {
        identity: "github:1431055733/protocol-fixture",
        required: &["status"],
        optional: &["context.terminals", "context.cwd"],
    };
    let mut calls = 0u64;
    managed::run(&profile, |scope, context| {
        calls += 1;
        let text = format!("{calls}:{}", scope.slot);
        let tooltip = context.get("terminals").map(ToString::to_string);
        Status {
            refresh_ms: Some(1000),
            fragments: vec![json!({"text":text,"tooltip":tooltip})],
        }
    })
}
