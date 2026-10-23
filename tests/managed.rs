use among_example_plugins::{managed, protocol::Status};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

fn hello() -> Value {
    json!({"type":"managed_hello","major":1,"minor":2,
        "identity":"github:1431055733/protocol-fixture","session":"session-one",
        "generation":"generation-one","features":["status"]})
}

fn status(id: u64) -> Value {
    json!({"type":"status","session":"session-one","generation":"generation-one",
        "request_id":id,"scope":{"slot":"app_footer","target":{"kind":"application"}},
        "context":{"session":"session-one"}})
}

struct Process {
    child: Child,
    input: Option<ChildStdin>,
    output: Receiver<Value>,
    reader: Option<std::thread::JoinHandle<()>>,
}

impl Process {
    fn start() -> Self {
        let executable = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("examples/managed-status");
        let mut child = Command::new(&executable)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap_or_else(|error| {
                panic!("build the managed-status example before this test: {executable:?}: {error}")
            });
        let input = child.stdin.take();
        let stdout = child.stdout.take().unwrap();
        let (sender, output) = mpsc::sync_channel(16);
        let reader = std::thread::spawn(move || {
            let mut stdout = BufReader::new(stdout);
            loop {
                let mut line = Vec::new();
                let count = stdout
                    .by_ref()
                    .take((managed::MAX_FRAME_BYTES + 1) as u64)
                    .read_until(b'\n', &mut line)
                    .unwrap();
                if count == 0 {
                    break;
                }
                assert!(count <= managed::MAX_FRAME_BYTES && line.last() == Some(&b'\n'));
                let message = serde_json::from_slice(&line).unwrap();
                sender
                    .try_send(message)
                    .expect("managed fixture stdout queue exceeded 16 frames");
            }
        });
        Self {
            child,
            input,
            output,
            reader: Some(reader),
        }
    }
    fn send(&mut self, value: Value) {
        self.raw(format!("{value}\n").as_bytes());
    }
    fn raw(&mut self, bytes: &[u8]) {
        self.input.as_mut().unwrap().write_all(bytes).unwrap();
        self.input.as_mut().unwrap().flush().unwrap();
    }
    fn receive(&self) -> Value {
        self.output
            .recv_timeout(Duration::from_secs(3))
            .expect("managed fixture reply deadline")
    }
    fn exited(&mut self, success: bool) {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert_eq!(status.success(), success, "child status: {status}");
                break;
            }
            assert!(Instant::now() < deadline, "managed child did not retire");
            std::thread::sleep(Duration::from_millis(2));
        }
        self.reader.take().unwrap().join().unwrap();
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        self.input.take();
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

#[test]
fn managed_admission_and_current_binding_reach_real_stdout_then_shutdown() {
    let mut process = Process::start();
    assert!(process
        .output
        .recv_timeout(Duration::from_millis(30))
        .is_err());
    let mut greeting = hello();
    greeting["features"] = json!(["status", "settings", "notifications.produce"]);
    process.send(greeting);
    assert_eq!(process.receive(), hello());
    for id in [1, 7, u64::MAX] {
        process.send(status(id));
        let reply = process.receive();
        assert_eq!(reply["type"], "status");
        assert_eq!(reply["session"], "session-one");
        assert_eq!(reply["generation"], "generation-one");
        assert_eq!(reply["request_id"], id);
        assert_eq!(reply["refresh_ms"], 1000);
        assert_eq!(
            reply["fragments"][0]["text"],
            format!(
                "{}:app_footer",
                if id == 1 {
                    1
                } else if id == 7 {
                    2
                } else {
                    3
                }
            )
        );
    }
    process.send(json!({"type":"shutdown"}));
    process.exited(true);
    assert!(process.output.try_recv().is_err());
}

#[test]
fn managed_required_grants_identity_and_version_refuse_without_output() {
    for (field, value) in [
        ("features", json!([])),
        ("features", json!(["status", "status"])),
        ("features", json!(["status", "context.cwd"])),
        ("identity", json!("github:999/protocol-fixture")),
        ("major", json!(2)),
        ("minor", json!(1)),
        ("session", json!("")),
        ("generation", json!("")),
    ] {
        let mut process = Process::start();
        let mut greeting = hello();
        greeting[field] = value;
        process.send(greeting);
        process.exited(false);
        assert!(
            process.output.try_recv().is_err(),
            "unexpected admission for {field}"
        );
    }
}

#[test]
fn managed_stale_bindings_and_replayed_ids_never_invoke_the_consumer() {
    let mut process = Process::start();
    process.send(hello());
    process.receive();
    for field in ["session", "generation"] {
        let mut stale = status(900);
        stale[field] = json!("retired");
        process.send(stale);
    }
    process.send(status(5));
    assert_eq!(process.receive()["fragments"][0]["text"], "1:app_footer");
    process.send(status(5));
    process.send(status(4));
    process.send(status(6));
    let reply = process.receive();
    assert_eq!(reply["request_id"], 6);
    assert_eq!(reply["fragments"][0]["text"], "2:app_footer");
    process.send(json!({"type":"shutdown"}));
    process.exited(true);
    assert!(process.output.try_recv().is_err());
}

#[test]
fn managed_scopes_and_granted_context_preserve_unicode_not_sibling_authority() {
    let mut process = Process::start();
    let mut greeting = hello();
    greeting["features"] = json!(["status", "context.terminals", "context.cwd"]);
    process.send(greeting);
    assert_eq!(
        process.receive()["features"],
        json!(["context.cwd", "context.terminals", "status"])
    );
    let mut request = status(1);
    request["scope"] =
        json!({"slot":"pane_footer","target":{"kind":"pane","reference":"18446744073709551615"}});
    request["context"] = json!({"session":"session-one","terminals":[
        {"pane":"18446744073709551615","workspace":"10","cwd":"/данные/日本語"}],"next_cursor":null});
    process.send(request.clone());
    assert!(process.receive()["fragments"][0]["tooltip"]
        .as_str()
        .unwrap()
        .contains("/данные/日本語"));
    request["request_id"] = json!(2);
    request["context"]["terminals"][0]["pane"] = json!("9");
    process.send(request);
    process.exited(false);
    assert!(process.output.try_recv().is_err());
}

#[test]
fn managed_rejects_preadmission_repeat_hello_and_malformed_reference_or_context() {
    let mut process = Process::start();
    process.send(status(1));
    process.exited(false);
    assert!(process.output.try_recv().is_err());
    let mut process = Process::start();
    process.send(hello());
    process.receive();
    process.send(hello());
    process.exited(false);
    assert!(process.output.try_recv().is_err());
    for reference in [
        json!(1),
        json!("01"),
        json!("+1"),
        json!("-1"),
        json!("18446744073709551616"),
    ] {
        let mut process = Process::start();
        process.send(hello());
        process.receive();
        let mut request = status(1);
        request["scope"] =
            json!({"slot":"pane_footer","target":{"kind":"pane","reference":reference}});
        process.send(request);
        process.exited(false);
        assert!(process.output.try_recv().is_err());
    }
    for context in [
        json!({"session":"other"}),
        json!({"session":"session-one","terminals":[]}),
    ] {
        let mut process = Process::start();
        process.send(hello());
        process.receive();
        let mut request = status(1);
        request["context"] = context;
        process.send(request);
        process.exited(false);
        assert!(process.output.try_recv().is_err());
    }
}

#[test]
fn managed_framing_is_bounded_before_parsing_and_never_emits_partial_replies() {
    let profile = managed::Profile {
        identity: "github:1431055733/protocol-fixture",
        required: &["status"],
        optional: &[],
    };
    for frame in [
        b"not JSON\n".to_vec(),
        b"\xff\n".to_vec(),
        b"{}".to_vec(),
        b"{\"type\":\"shutdown\",\"type\":\"status\"}\n".to_vec(),
        b"{\"type\":\"shutdown\",\"nested\":{\"id\":1,\"\\u0069d\":2}}\n".to_vec(),
        format!("{}0{}\n", "[".repeat(33), "]".repeat(33)).into_bytes(),
        vec![b'x'; managed::MAX_FRAME_BYTES],
    ] {
        let mut output = Vec::new();
        let result = managed::serve(&profile, frame.as_slice(), &mut output, |_, _| {
            panic!("unauthorized callback")
        });
        assert!(result.is_err());
        assert!(output.is_empty());
    }
    let input = format!("{}\n{}\n", hello(), status(1));
    let mut output = Vec::new();
    assert!(
        managed::serve(&profile, input.as_bytes(), &mut output, |_, _| Status {
            refresh_ms: None,
            fragments: vec![json!({"text":"x".repeat(managed::MAX_FRAME_BYTES)})]
        })
        .is_err()
    );
    assert_eq!(output, format!("{}\n", hello()).as_bytes());
    let mut exact = serde_json::to_vec(&hello()).unwrap();
    exact.resize(managed::MAX_FRAME_BYTES - 1, b' ');
    exact.push(b'\n');
    let mut output = Vec::new();
    managed::serve(
        &profile,
        exact.as_slice(),
        &mut output,
        |_, _| unreachable!(),
    )
    .unwrap();
    assert_eq!(output, format!("{}\n", hello()).as_bytes());
    exact.insert(exact.len() - 1, b' ');
    assert!(managed::serve(
        &profile,
        exact.as_slice(),
        Vec::new(),
        |_, _| unreachable!()
    )
    .is_err());
}

#[test]
fn managed_real_process_eof_and_unterminated_input_retire_without_replay() {
    let mut process = Process::start();
    process.send(hello());
    process.receive();
    process.input.take();
    process.exited(true);
    assert!(process.output.try_recv().is_err());
    let mut process = Process::start();
    process.raw(b"{\"type\":\"shutdown\"}");
    process.input.take();
    process.exited(false);
    assert!(process.output.try_recv().is_err());
}

#[test]
fn managed_real_process_rejects_malformed_deep_duplicate_and_oversized_frames() {
    for frame in [
        b"not JSON\n".to_vec(),
        b"\xff\n".to_vec(),
        b"{\"type\":\"shutdown\",\"type\":\"status\"}\n".to_vec(),
        format!("{}0{}\n", "[".repeat(33), "]".repeat(33)).into_bytes(),
        vec![b' '; managed::MAX_FRAME_BYTES],
    ] {
        let mut process = Process::start();
        process.raw(&frame);
        process.exited(false);
        assert!(process.output.try_recv().is_err());
    }
}

#[test]
fn managed_context_refusals_do_not_call_or_reply_and_unavailable_stays_null() {
    let profile = managed::Profile {
        identity: "github:1431055733/protocol-fixture",
        required: &["status"],
        optional: &["context.terminals", "context.cwd"],
    };
    let base = json!({"session":"session-one","terminals":[{"pane":"1","workspace":"10","cwd":null,"unavailable":["cwd"]}],"next_cursor":null});
    let mut cases = Vec::new();
    let mut value = base.clone();
    value["title"] = json!("undeclared");
    cases.push(value);
    let mut value = base.clone();
    value["terminals"][0]["title"] = json!("undeclared");
    cases.push(value);
    let mut value = base.clone();
    value["terminals"][0]["cwd"] = json!("x".repeat(4097));
    value["terminals"][0]
        .as_object_mut()
        .unwrap()
        .remove("unavailable");
    cases.push(value);
    let mut value = base.clone();
    value["terminals"][0]["cwd"] = json!("\u{1}".repeat(1400));
    value["terminals"][0]
        .as_object_mut()
        .unwrap()
        .remove("unavailable");
    cases.push(value);
    let mut value = base.clone();
    value["terminals"][0]["unavailable"] = json!(["title"]);
    cases.push(value);
    let mut value = base.clone();
    value["terminals"][0]["unavailable"] = json!(["cwd", "cwd"]);
    cases.push(value);
    let mut value = base.clone();
    value["terminals"][0]["cwd"] = json!("/known");
    cases.push(value);
    let mut value = base.clone();
    value["terminals"] = json!([base["terminals"][0], base["terminals"][0]]);
    cases.push(value);
    let mut value = base.clone();
    value["terminals"][0]["workspace"] = json!("11");
    cases.push(value);
    let mut value = base.clone();
    value["next_cursor"] = json!("01");
    cases.push(value);
    for context in cases {
        let mut greeting = hello();
        greeting["features"] = json!(["status", "context.terminals", "context.cwd"]);
        let mut request = status(1);
        request["scope"] =
            json!({"slot":"workspace_summary","target":{"kind":"workspace","reference":"10"}});
        request["context"] = context;
        let input = format!("{greeting}\n{request}\n");
        let mut output = Vec::new();
        assert!(
            managed::serve(&profile, input.as_bytes(), &mut output, |_, _| panic!(
                "context bypass"
            ))
            .is_err()
        );
        assert_eq!(output.iter().filter(|byte| **byte == b'\n').count(), 1);
    }
    for (count, length) in [(33, 0), (8, 4096)] {
        let mut context = base.clone();
        context["terminals"] = Value::Array(
            (1..=count)
                .map(|pane| json!({"pane":pane.to_string(),"cwd":"x".repeat(length)}))
                .collect(),
        );
        let mut greeting = hello();
        greeting["features"] = json!(["status", "context.terminals", "context.cwd"]);
        let mut request = status(1);
        request["context"] = context;
        let mut output = Vec::new();
        assert!(managed::serve(
            &profile,
            format!("{greeting}\n{request}\n").as_bytes(),
            &mut output,
            |_, _| panic!("page bypass")
        )
        .is_err());
        assert_eq!(output.iter().filter(|byte| **byte == b'\n').count(), 1);
    }
    let mut greeting = hello();
    greeting["features"] = json!(["status", "context.terminals"]);
    let mut request = status(1);
    request["context"] = base.clone();
    request["context"]["terminals"][0]
        .as_object_mut()
        .unwrap()
        .remove("unavailable");
    let mut output = Vec::new();
    assert!(managed::serve(
        &profile,
        format!("{greeting}\n{request}\n").as_bytes(),
        &mut output,
        |_, _| panic!("grant bypass")
    )
    .is_err());
    assert_eq!(output.iter().filter(|byte| **byte == b'\n').count(), 1);
    greeting["features"] = json!(["status", "context.terminals", "context.cwd"]);
    request["context"] = base.clone();
    let mut called = 0;
    managed::serve(
        &profile,
        format!("{greeting}\n{request}\n").as_bytes(),
        Vec::new(),
        |_, context| {
            called += 1;
            assert!(context["terminals"][0]["cwd"].is_null());
            Status::empty(None)
        },
    )
    .unwrap();
    assert_eq!(called, 1);
    for path in ["x".repeat(4096), "\u{1}".repeat(1365)] {
        request["context"]["terminals"][0]["cwd"] = json!(path);
        request["context"]["terminals"][0]
            .as_object_mut()
            .unwrap()
            .remove("unavailable");
        let mut called = 0;
        managed::serve(
            &profile,
            format!("{greeting}\n{request}\n").as_bytes(),
            Vec::new(),
            |_, context| {
                called += 1;
                assert_eq!(context["terminals"][0]["cwd"], path);
                Status::empty(None)
            },
        )
        .unwrap();
        assert_eq!(called, 1);
    }
}

#[test]
fn managed_profile_validation_and_pipe_errors_are_not_false_success() {
    for (required, optional) in [
        (vec![], vec![]),
        (vec!["status", "settings"], vec![]),
        (vec!["status"], vec!["status"]),
        (vec!["status", "context.cwd"], vec![]),
    ] {
        let profile = managed::Profile {
            identity: "github:1431055733/protocol-fixture",
            required: &required,
            optional: &optional,
        };
        let mut output = Vec::new();
        assert!(managed::serve(
            &profile,
            format!("{}\n", hello()).as_bytes(),
            &mut output,
            |_, _| panic!("profile bypass")
        )
        .is_err());
        assert!(output.is_empty());
    }
    struct Closed;
    impl Write for Closed {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            panic!("failed write must not flush")
        }
    }
    let profile = managed::Profile {
        identity: "github:1431055733/protocol-fixture",
        required: &["status"],
        optional: &[],
    };
    assert_eq!(
        managed::serve(
            &profile,
            format!("{}\n", hello()).as_bytes(),
            Closed,
            |_, _| panic!("failed admission")
        )
        .unwrap_err()
        .kind(),
        std::io::ErrorKind::BrokenPipe
    );
}
