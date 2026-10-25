use among_example_plugins::managed;
use rustix::fs::{fcntl_getfl, fcntl_setfl, OFlags};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

fn hello() -> Value {
    json!({"type":"managed_hello","major":1,"minor":2,"identity":"github:1431055733/sdk-pages-fixture",
        "session":"paging-session","generation":"paging-generation","features":["context.cwd","context.terminals","status"]})
}

fn scope(workspace: Option<u64>) -> Value {
    workspace.map_or_else(|| json!({"slot":"app_header","target":{"kind":"application"}}),
        |id| json!({"slot":"workspace_summary","target":{"kind":"workspace","reference":id.to_string()}}))
}

fn context(
    workspace: Option<u64>,
    start: u64,
    count: u64,
    cursor: Option<u64>,
    long: bool,
) -> Value {
    let cwd = if long {
        "/".to_string() + &"界".repeat(999)
    } else {
        "/private-paging-path".into()
    };
    let terminals: Vec<_> = (start..start + count)
        .map(|pane| {
            json!({"pane":pane.to_string(),
        "workspace":workspace.unwrap_or(10).to_string(),"cwd":cwd})
        })
        .collect();
    let value = json!({"session":"paging-session","terminals":terminals,"next_cursor":cursor.map(|id| id.to_string())});
    assert!(
        serde_json::to_vec(&value["terminals"]).unwrap().len() <= managed::MAX_CONTEXT_PAGE_BYTES
    );
    value
}

fn status(id: u64, scope: Value, context: Value) -> Value {
    json!({"type":"status","session":"paging-session","generation":"paging-generation","request_id":id,"scope":scope,"context":context})
}

fn page(enumeration: &Value, context: Value) -> Value {
    json!({"type":"context_page","session":"paging-session","generation":"paging-generation",
        "request_id":enumeration["request_id"],"scope":enumeration["scope"],"context":context})
}

struct Process {
    child: Child,
    input: Option<ChildStdin>,
    output: Receiver<Value>,
    reader: Option<std::thread::JoinHandle<()>>,
}

impl Process {
    fn start(arguments: &[&str]) -> Self {
        let executable = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("examples/managed-pages");
        let mut child = Command::new(&executable)
            .args(arguments)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap_or_else(|error| {
                panic!("build managed-pages before process tests: {executable:?}: {error}")
            });
        let input = child.stdin.take().unwrap();
        fcntl_setfl(&input, fcntl_getfl(&input).unwrap() | OFlags::NONBLOCK).unwrap();
        let stdout = child.stdout.take().unwrap();
        let (sender, output) = mpsc::sync_channel(16);
        let reader = std::thread::spawn(move || {
            let mut stdout = BufReader::new(stdout);
            loop {
                let mut bytes = Vec::new();
                let count = stdout
                    .by_ref()
                    .take((managed::MAX_FRAME_BYTES + 1) as u64)
                    .read_until(b'\n', &mut bytes)
                    .unwrap();
                if count == 0 {
                    break;
                }
                assert!(count <= managed::MAX_FRAME_BYTES && bytes.last() == Some(&b'\n'));
                sender
                    .try_send(serde_json::from_slice(&bytes).unwrap())
                    .expect("bounded paging stdout queue");
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
            assert!(Instant::now() < deadline, "paging stdin deadline");
            match self.input.as_mut().unwrap().write(bytes) {
                Ok(0) => panic!("paging stdin closed"),
                Ok(count) => bytes = &bytes[count..],
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) =>
                {
                    std::thread::sleep(Duration::from_millis(2))
                }
                Err(error) => panic!("paging stdin: {error}"),
            }
        }
    }

    fn send(&mut self, value: Value) {
        self.raw(format!("{value}\n").as_bytes());
    }
    fn receive(&self) -> Value {
        self.output
            .recv_timeout(Duration::from_secs(3))
            .expect("paging stdout deadline")
    }
    fn admit(&mut self) {
        self.send(hello());
        assert_eq!(self.receive(), hello());
    }
    fn enumerate(&self, expected_scope: &Value, expected_cursor: u64) -> Value {
        let row = self.receive();
        assert_eq!(row["type"], "enumerate");
        assert_eq!(row["session"], "paging-session");
        assert_eq!(row["generation"], "paging-generation");
        assert_eq!(row["scope"], *expected_scope);
        assert_eq!(row["cursor"], expected_cursor.to_string());
        row
    }
    fn count(&self, id: u64, count: u64) {
        let row = self.receive();
        assert_eq!(row["type"], "status");
        assert_eq!(row["request_id"], id);
        assert_eq!(row["session"], "paging-session");
        assert_eq!(row["generation"], "paging-generation");
        assert_eq!(row["fragments"][0]["text"], count.to_string());
    }
    fn exited(&mut self, success: bool) {
        let deadline = Instant::now() + Duration::from_secs(6);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert_eq!(status.success(), success, "{status}");
                break;
            }
            assert!(Instant::now() < deadline, "paging child did not exit");
            std::thread::sleep(Duration::from_millis(2));
        }
        let reader = self.reader.take().unwrap();
        while !reader.is_finished() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
        reader.join().unwrap();
        assert!(
            self.output.try_recv().is_err(),
            "no invented completion or extra enumeration"
        );
        let mut diagnostics = Vec::new();
        self.child
            .stderr
            .take()
            .unwrap()
            .take(129)
            .read_to_end(&mut diagnostics)
            .unwrap();
        assert!(diagnostics.len() <= 128);
        let diagnostics = String::from_utf8(diagnostics).unwrap();
        assert!(
            !diagnostics.contains("paging-session") && !diagnostics.contains("private-paging-path")
        );
        if success {
            assert!(diagnostics.is_empty());
        } else {
            assert!(diagnostics.starts_with("managed transport failed:"));
        }
    }
    fn shutdown(&mut self) {
        self.send(json!({"type":"shutdown"}));
        self.exited(true);
        self.input.take();
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
fn real_sdk_count_and_byte_pages_reduce_application_and_workspace_without_concatenation() {
    for (workspace, width, pages) in [(None, 32, 2), (Some(10), 8, 3)] {
        let mut child = Process::start(&[]);
        child.admit();
        let target = scope(workspace);
        child.send(status(
            7,
            target.clone(),
            context(workspace, 1, width, Some(width), workspace.is_some()),
        ));
        for index in 1..pages {
            let enumeration = child.enumerate(&target, index * width);
            assert_eq!(enumeration["request_id"], index);
            let last = index + 1 == pages;
            child.send(page(
                &enumeration,
                context(
                    workspace,
                    index * width + 1,
                    if last { 1 } else { width },
                    if last {
                        None
                    } else {
                        Some((index + 1) * width)
                    },
                    workspace.is_some(),
                ),
            ));
        }
        child.count(7, (pages - 1) * width + 1);
        child.shutdown();
    }
}

#[test]
fn real_sdk_page_ids_never_advance_status_ids_and_unrelated_pane_requests_keep_working() {
    let mut child = Process::start(&[]);
    child.admit();
    let application = scope(None);
    child.send(status(
        7,
        application.clone(),
        context(None, 1, 32, Some(32), false),
    ));
    let enumeration = child.enumerate(&application, 32);
    assert_eq!(enumeration["request_id"], 1);
    let pane = json!({"slot":"pane_footer","target":{"kind":"pane","reference":"9"}});
    child.send(status(8, pane, context(None, 9, 1, None, false)));
    let pane = child.receive();
    assert_eq!(pane["request_id"], 8);
    assert_eq!(pane["fragments"][0]["tooltip"], "/private-paging-path");
    child.send(page(&enumeration, context(None, 33, 8, None, false)));
    child.count(7, 40);
    child.send(status(
        8,
        application.clone(),
        context(None, 1, 1, None, false),
    ));
    child.send(status(9, application, context(None, 99, 1, None, false)));
    child.count(9, 1);
    child.shutdown();
}

#[test]
fn real_sdk_missing_partial_and_unterminated_continuations_expire_with_stdin_open() {
    for partial in [
        b"".as_slice(),
        b"{\"type\":\"context_page\",",
        b"{\"type\":\"shutdown\"}",
    ] {
        let mut child = Process::start(&[]);
        child.admit();
        let target = scope(None);
        let started = Instant::now();
        child.send(status(
            7,
            target.clone(),
            context(None, 1, 1, Some(1), false),
        ));
        child.enumerate(&target, 1);
        child.raw(partial);
        child.exited(false);
        assert!(started.elapsed() >= Duration::from_millis(3500));
        assert!(started.elapsed() < Duration::from_millis(5500));
        assert!(child.input.is_some(), "EOF is not the timeout witness");
    }
}

#[test]
fn real_sdk_absolute_budget_is_not_renewed_by_successful_progress() {
    let mut child = Process::start(&[]);
    child.admit();
    let target = scope(None);
    let started = Instant::now();
    child.send(status(
        7,
        target.clone(),
        context(None, 1, 1, Some(1), false),
    ));
    let first = child.enumerate(&target, 1);
    std::thread::sleep(Duration::from_millis(2100));
    child.send(page(&first, context(None, 2, 1, Some(2), false)));
    child.enumerate(&target, 2);
    child.exited(false);
    assert!(started.elapsed() >= Duration::from_millis(3500));
    assert!(started.elapsed() < Duration::from_millis(5500));
}

#[test]
fn real_sdk_late_and_duplicate_continuations_cannot_publish_or_replay_results() {
    let mut child = Process::start(&[]);
    child.admit();
    let target = scope(None);
    child.send(status(
        7,
        target.clone(),
        context(None, 1, 1, Some(1), false),
    ));
    let enumeration = child.enumerate(&target, 1);
    std::thread::sleep(Duration::from_millis(4250));
    let late = format!("{}\n", page(&enumeration, context(None, 2, 1, None, false)));
    let written = child.input.as_mut().unwrap().write_all(late.as_bytes());
    assert!(
        written.is_ok()
            || written
                .as_ref()
                .is_err_and(|error| error.kind() == std::io::ErrorKind::BrokenPipe)
    );
    child.exited(false);

    let mut child = Process::start(&[]);
    child.admit();
    child.send(status(
        7,
        target.clone(),
        context(None, 1, 1, Some(1), false),
    ));
    let enumeration = child.enumerate(&target, 1);
    let response = page(&enumeration, context(None, 2, 1, None, false));
    child.send(response.clone());
    child.count(7, 2);
    child.send(response);
    child.exited(false);
}

#[test]
fn real_sdk_shutdown_cancels_traversal_but_eof_does_not_invent_success() {
    for shutdown in [true, false] {
        let mut child = Process::start(&[]);
        child.admit();
        let target = scope(Some(10));
        child.send(status(
            7,
            target.clone(),
            context(Some(10), 1, 1, Some(1), false),
        ));
        child.enumerate(&target, 1);
        if shutdown {
            child.shutdown();
        } else {
            child.input.take();
            child.exited(false);
        }
    }
}

#[test]
fn real_sdk_refuses_wrong_or_unsolicited_pages_before_consumption() {
    for field in [
        "request_id",
        "session",
        "generation",
        "scope",
        "context",
        "unsolicited",
    ] {
        let mut child = Process::start(&[]);
        child.admit();
        let target = scope(None);
        let enumeration = if field == "unsolicited" {
            json!({"request_id":1,"scope":target})
        } else {
            child.send(status(
                7,
                target.clone(),
                context(None, 1, 1, Some(1), false),
            ));
            child.enumerate(&target, 1)
        };
        let mut response = page(&enumeration, context(None, 2, 1, None, false));
        match field {
            "request_id" => response[field] = json!(999),
            "session" | "generation" => response[field] = json!("stale"),
            "scope" => response[field] = scope(Some(10)),
            "context" => response[field]["session"] = json!("stale"),
            _ => {}
        }
        child.send(response);
        child.exited(false);
    }
}

#[test]
fn real_sdk_one_enumeration_and_fifteen_correlations_are_enforced() {
    let mut child = Process::start(&[]);
    child.admit();
    for index in 0..managed::MAX_PENDING_STATUS {
        let workspace = Some(10 + index as u64);
        let target = scope(workspace);
        child.send(status(
            index as u64 + 1,
            target.clone(),
            context(workspace, 1, 1, Some(1), false),
        ));
        if index == 0 {
            child.enumerate(&target, 1);
        }
    }
    let workspace = Some(99);
    child.send(status(
        16,
        scope(workspace),
        context(workspace, 1, 1, Some(1), false),
    ));
    child.exited(false);
}

#[test]
fn real_sdk_encoded_state_is_refused_before_enumeration() {
    let mut child = Process::start(&["--oversize-state"]);
    child.admit();
    child.send(status(7, scope(None), context(None, 1, 1, Some(1), false)));
    child.exited(false);
}
