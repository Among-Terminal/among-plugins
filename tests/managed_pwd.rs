use among_example_plugins::managed;
use rustix::fs::{fcntl_getfl, fcntl_setfl, OFlags};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

fn hello() -> Value {
    json!({"type":"managed_hello","major":1,"minor":2,
        "identity":"github:1431055733/pane-pwd","session":"pwd-session",
        "generation":"pwd-generation","features":["context.cwd","context.terminals","status"]})
}

fn request(id: u64, pane: &str, terminals: Value) -> Value {
    json!({"type":"status","session":"pwd-session","generation":"pwd-generation",
        "request_id":id,"scope":{"slot":"pane_footer","target":{"kind":"pane","reference":pane}},
        "context":{"session":"pwd-session","terminals":terminals,"next_cursor":null}})
}

fn terminal(pane: &str, cwd: Value) -> Value {
    json!({"pane":pane,"workspace":"10","cwd":cwd})
}

struct Pwd {
    child: Child,
    input: Option<ChildStdin>,
    output: Receiver<Value>,
    reader: Option<std::thread::JoinHandle<()>>,
}

impl Pwd {
    fn start() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_pane-pwd-managed"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let flags = fcntl_getfl(&input).unwrap();
        fcntl_setfl(&input, flags | OFlags::NONBLOCK).unwrap();
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
                sender
                    .try_send(serde_json::from_slice(&line).unwrap())
                    .expect("pane-pwd stdout exceeded its bounded queue");
            }
        });
        Self {
            child,
            input: Some(input),
            output,
            reader: Some(reader),
        }
    }

    fn raw(&mut self, mut bytes: &[u8]) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while !bytes.is_empty() {
            assert!(Instant::now() < deadline, "pane-pwd stdin write deadline");
            match self.input.as_mut().unwrap().write(bytes) {
                Ok(0) => panic!("pane-pwd stdin closed before its frame was written"),
                Ok(count) => bytes = &bytes[count..],
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) =>
                {
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(error) => panic!("pane-pwd stdin write failed: {error}"),
            }
        }
    }

    fn send(&mut self, value: Value) {
        self.raw(format!("{value}\n").as_bytes());
    }

    fn receive(&self) -> Value {
        self.output
            .recv_timeout(Duration::from_secs(3))
            .expect("actual pane-pwd reply deadline")
    }

    fn admit(&mut self) {
        self.send(hello());
        assert_eq!(self.receive(), hello());
    }

    fn status(&mut self, input: Value, path: Option<&str>) {
        let id = input["request_id"].clone();
        self.send(input);
        let row = self.receive();
        assert_eq!(row["type"], "status");
        assert_eq!(row["session"], "pwd-session");
        assert_eq!(row["generation"], "pwd-generation");
        assert_eq!(row["request_id"], id);
        assert_eq!(row["refresh_ms"], 1000);
        let expected = match path {
            Some(path) => json!([{"text":path.chars().take(480).collect::<String>(),
                "tooltip":path,"priority":10}]),
            None => json!([]),
        };
        assert_eq!(row["fragments"], expected);
    }

    fn exited(&mut self, success: bool) {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert_eq!(status.success(), success, "pane-pwd exited: {status}");
                break;
            }
            assert!(Instant::now() < deadline, "pane-pwd did not exit");
            std::thread::sleep(Duration::from_millis(2));
        }
        let reader = self.reader.take().unwrap();
        while !reader.is_finished() {
            assert!(
                Instant::now() < deadline,
                "pane-pwd stdout reader did not exit"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        reader.join().unwrap();
        assert!(self.output.try_recv().is_err());
    }

    fn shutdown(&mut self) {
        self.send(json!({"type":"shutdown"}));
        self.exited(true);
        self.input.take();
    }
}

impl Drop for Pwd {
    fn drop(&mut self) {
        self.input.take();
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

#[test]
fn managed_pwd_requires_exact_three_grants_and_preserves_binding_and_shutdown() {
    let mut pwd = Pwd::start();
    assert!(matches!(
        pwd.output.recv_timeout(Duration::from_millis(30)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    let mut greeting = hello();
    greeting["features"] = json!([
        "status",
        "settings",
        "events",
        "context.terminals",
        "context.cwd"
    ]);
    pwd.send(greeting);
    assert_eq!(pwd.receive(), hello());
    let cwd = json!([terminal("32", json!("/hidden"))]);
    pwd.status(request(1, "32", cwd.clone()), Some("/hidden"));
    for (field, stale_id, live_id) in [("session", 1000, 2), ("generation", 2000, 3)] {
        let mut stale = request(stale_id, "32", cwd.clone());
        stale[field] = json!("stale");
        pwd.send(stale);
        pwd.status(request(live_id, "32", cwd.clone()), Some("/hidden"));
    }
    pwd.send(request(2, "32", cwd.clone()));
    pwd.send(request(3, "32", cwd.clone()));
    pwd.status(request(4, "32", cwd), Some("/hidden"));
    pwd.shutdown();
}

#[test]
fn managed_pwd_uses_only_requested_pane_and_shared_unicode_renderer() {
    let mut pwd = Pwd::start();
    pwd.admit();
    let unicode = format!("/資料/{}", "🙂é界".repeat(180));
    for (id, pane, path) in [
        (1, "31", "/focused"),
        (2, "32", unicode.as_str()),
        (3, "0", "/zero"),
    ] {
        pwd.status(
            request(id, pane, json!([terminal(pane, json!(path))])),
            Some(path),
        );
    }
    for (offset, (slot, target)) in [
        ("pane_header", json!({"kind":"pane","reference":"32"})),
        ("app_header", json!({"kind":"application"})),
        ("app_footer", json!({"kind":"application"})),
        (
            "workspace_header",
            json!({"kind":"workspace","reference":"10"}),
        ),
        (
            "workspace_summary",
            json!({"kind":"workspace","reference":"10"}),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let mut input = request(
            4 + offset as u64,
            "32",
            json!([terminal("32", json!("/not-a-footer"))]),
        );
        input["scope"] = json!({"slot":slot,"target":target});
        pwd.status(input, None);
    }
    pwd.shutdown();
}

#[test]
fn managed_pwd_missing_unknown_empty_and_unavailable_cwd_never_fall_back() {
    let mut pwd = Pwd::start();
    pwd.admit();
    for (offset, terminals) in [
        json!([]),
        json!([{"pane":"32","workspace":"10"}]),
        json!([terminal("32", Value::Null)]),
        json!([terminal("32", json!(""))]),
        json!([{"pane":"32","workspace":"10","cwd":null,"unavailable":["cwd"]}]),
    ]
    .into_iter()
    .enumerate()
    {
        pwd.status(request(1 + offset as u64, "32", terminals), None);
    }
    pwd.input.take();
    pwd.exited(true);
}

#[test]
fn managed_pwd_refuses_missing_grants_and_legacy_or_malformed_admission() {
    let mut greetings = Vec::new();
    for missing in ["status", "context.terminals", "context.cwd"] {
        let mut greeting = hello();
        greeting["features"]
            .as_array_mut()
            .unwrap()
            .retain(|feature| feature != missing);
        greetings.push(format!("{greeting}\n").into_bytes());
    }
    for (field, value) in [
        ("identity", json!("github:1431055733/clock")),
        ("minor", json!(0)),
    ] {
        let mut greeting = hello();
        greeting[field] = value;
        greetings.push(format!("{greeting}\n").into_bytes());
    }
    greetings.extend([
        b"{\"type\":\"hello\",\"major\":1,\"minor\":0}\n".to_vec(),
        b"{\"type\":\"shutdown\",\"type\":\"managed_hello\"}\n".to_vec(),
        b"not JSON\n".to_vec(),
        b"{\"type\":".to_vec(),
    ]);
    for greeting in greetings {
        let mut pwd = Pwd::start();
        pwd.raw(&greeting);
        pwd.input.take();
        pwd.exited(false);
    }
}

#[test]
fn managed_pwd_refuses_noncanonical_sibling_duplicate_or_undeclared_context() {
    let baseline = request(1, "32", json!([terminal("32", json!("/known"))]));
    let mut inputs = Vec::new();
    for reference in [
        json!(32),
        json!("032"),
        json!("+32"),
        json!("18446744073709551616"),
    ] {
        let mut input = baseline.clone();
        input["scope"]["target"]["reference"] = reference;
        inputs.push(input);
    }
    for terminals in [
        json!([terminal("31", json!("/sibling"))]),
        json!([terminal("32", json!("/a")), terminal("32", json!("/b"))]),
        json!([{"pane":32,"cwd":"/numeric"}]),
        json!([{"pane":"32","cwd":"/known","title":"undeclared"}]),
    ] {
        let mut input = baseline.clone();
        input["context"]["terminals"] = terminals;
        inputs.push(input);
    }
    for field in ["focused_pane", "active_workspace"] {
        let mut input = baseline.clone();
        input["context"][field] = Value::Null;
        inputs.push(input);
    }
    let mut wrong_context_session = baseline;
    wrong_context_session["context"]["session"] = json!("other-session");
    inputs.push(wrong_context_session);
    for input in inputs {
        let mut pwd = Pwd::start();
        pwd.admit();
        pwd.send(input);
        pwd.input.take();
        pwd.exited(false);
    }
}

#[test]
fn managed_pwd_enforces_utf8_and_encoded_cwd_limits_at_actual_stdio() {
    for path in ["界".repeat(1365) + "/", "\n".repeat(4095)] {
        assert!(path.len() <= 4096);
        assert!(serde_json::to_vec(&path).unwrap().len() <= 8192);
        let mut pwd = Pwd::start();
        pwd.admit();
        pwd.status(
            request(1, "32", json!([terminal("32", json!(path))])),
            Some(&path),
        );
        pwd.shutdown();
    }
    for path in ["界".repeat(1365) + "//", "\n".repeat(4095) + "a"] {
        assert!(path.len() > 4096 || serde_json::to_vec(&path).unwrap().len() > 8192);
        let mut pwd = Pwd::start();
        pwd.admit();
        pwd.send(request(1, "32", json!([terminal("32", json!(path))])));
        pwd.input.take();
        pwd.exited(false);
    }
}
