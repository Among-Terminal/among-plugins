use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

fn replies(binary: &str, scopes: &[&str], context: Value) -> Vec<Value> {
    let mut child = Command::new(binary)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    writeln!(input, "not JSON").unwrap();
    writeln!(input, "{}", json!({"type":"hello","major":1,"minor":0})).unwrap();
    for (id, scope) in scopes.iter().enumerate() {
        writeln!(
            input,
            "{}",
            json!({"type":"status","request_id":id,"scope":scope,"context":context})
        )
        .unwrap();
    }
    writeln!(input, "{}", json!({"type":"shutdown"})).unwrap();
    drop(input);
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let mut rows = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(rows.remove(0), json!({"type":"hello","major":1,"minor":0}));
    for (id, row) in rows.iter().enumerate() {
        assert_eq!(row["request_id"], id);
        assert_eq!(row["refresh_ms"], 1000);
    }
    rows
}

#[test]
fn pwd_belongs_to_requested_pane_even_in_inactive_workspace() {
    let long = format!("/{}", "目录".repeat(300));
    let context = json!({"focused_pane":1,"active_workspace":0,"terminals":[
        {"pane":1,"workspace":0,"cwd":"/one"},{"pane":2,"workspace":1,"cwd":"/two"},
        {"pane":3,"cwd":null},{"pane":4,"cwd":long},
        {"pane":5,"cwd":""},{"pane":6}]});
    let rows = replies(
        env!("CARGO_BIN_EXE_pane-pwd"),
        &[
            "pane_footer:1",
            "pane_footer:2",
            "pane_footer:3",
            "pane_footer:4",
            "pane_footer:999",
            "pane_footer:-1",
            "pane_footer:x",
            "app_header",
            "pane_footer:5",
            "pane_footer:6",
        ],
        context,
    );
    assert_eq!(rows[0]["fragments"][0]["text"], "/one");
    assert_eq!(rows[1]["fragments"][0]["text"], "/two");
    assert_eq!(rows[2]["fragments"], serde_json::json!([]));
    assert_eq!(rows[3]["fragments"][0]["tooltip"], long);
    assert!(
        rows[3]["fragments"][0]["text"]
            .as_str()
            .unwrap()
            .chars()
            .count()
            <= 512
    );
    for row in &rows[4..] {
        assert_eq!(row["fragments"], json!([]));
    }
}

#[test]
fn resources_only_emit_header() {
    let rows = replies(
        env!("CARGO_BIN_EXE_system-status"),
        &["app_header", "pane_footer:1"],
        json!({}),
    );
    assert_eq!(rows[0]["fragments"].as_array().unwrap().len(), 3);
    assert_eq!(rows[0]["fragments"][0]["text"], "CPU N/A");
    assert_eq!(rows[1]["fragments"], json!([]));
}

#[test]
fn clock_uses_timezone_and_advances_over_actual_stdio() {
    for zone in ["UTC", "Asia/Tokyo"] {
        let mut child = Command::new(env!("CARGO_BIN_EXE_clock"))
            .env("TZ", zone)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut input = child.stdin.take().unwrap();
        let mut output = BufReader::new(child.stdout.take().unwrap());
        let mut observed = Vec::new();
        for id in 0..2 {
            let before = chrono::Utc::now();
            writeln!(
                input,
                "{}",
                json!({"type":"status","request_id":id,"scope":"app_footer","context":{}})
            )
            .unwrap();
            let mut line = String::new();
            output.read_line(&mut line).unwrap();
            let row: Value = serde_json::from_str(&line).unwrap();
            let text = row["fragments"][0]["text"].as_str().unwrap();
            let offset = if zone == "UTC" { 0 } else { 9 };
            let expected = (before + chrono::Duration::hours(offset))
                .format("%H:%M:%S")
                .to_string();
            let after = (chrono::Utc::now() + chrono::Duration::hours(offset))
                .format("%H:%M:%S")
                .to_string();
            assert!(text == expected || text == after, "{zone}: {text}");
            observed.push(text.to_owned());
            std::thread::sleep(std::time::Duration::from_millis(1100));
        }
        assert_ne!(observed[0], observed[1]);
        drop(input);
        assert!(child.wait().unwrap().success());
    }
    let rows = replies(
        env!("CARGO_BIN_EXE_clock"),
        &["pane_header:1", "app_header", "app_footer"],
        json!({}),
    );
    assert_eq!(rows[0]["fragments"], json!([]));
    assert_eq!(rows[1]["fragments"], json!([]));
    assert_eq!(rows[2]["fragments"].as_array().unwrap().len(), 1);
}
