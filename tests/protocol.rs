use serde_json::{json, Value};
use std::io::Write;
use std::process::{Command, Stdio};

#[test]
fn all_binaries_handshake_ignore_bad_input_and_stop_on_shutdown() {
    for binary in [
        env!("CARGO_BIN_EXE_clock"),
        env!("CARGO_BIN_EXE_system-status"),
        env!("CARGO_BIN_EXE_pane-pwd"),
    ] {
        let mut child = Command::new(binary)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut input = child.stdin.take().unwrap();
        for line in [
            "".into(),
            "invalid JSON".into(),
            "42".into(),
            json!({"type":"hello","major":1,"minor":0}).to_string(),
            json!({"type":"event", "event":{"kind":"pane_opened"}, "pane":1}).to_string(),
            json!({"type":"status", "request_id":9,"scope":"workspace_header","context":{}})
                .to_string(),
            json!({"type":"shutdown"}).to_string(),
            json!({"type":"hello"}).to_string(),
        ] {
            writeln!(input, "{line}").unwrap();
        }
        drop(input);
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success());
        let replies: Vec<Value> = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(replies.len(), 2);
        assert_eq!(replies[0], json!({"type":"hello","major":1,"minor":0}));
        assert_eq!(replies[1]["request_id"], 9);
        assert_eq!(replies[1]["fragments"], json!([]));
    }
}
