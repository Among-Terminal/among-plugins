use among_example_plugins::protocol::{run, Status};
use serde_json::json;

fn main() -> std::process::ExitCode {
    run(|scope, _context| Status {
        refresh_ms: Some(2000),
        fragments: if scope == "app_header" {
            vec![json!({"text": "Hello", "priority": 10})]
        } else {
            Vec::new()
        },
    })
}
