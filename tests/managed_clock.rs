use among_example_plugins::managed;
use chrono::Timelike;
use rustix::fs::{fcntl_getfl, fcntl_setfl, OFlags};
use serde_json::{json, Value};
use std::ffi::{OsStr, OsString};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::ffi::OsStringExt;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

fn hello() -> Value {
    json!({"type":"managed_hello","major":1,"minor":2,
        "identity":"github:1431055733/clock","session":"clock-session",
        "generation":"clock-generation","features":["status"]})
}

fn request(id: u64, slot: &str) -> Value {
    json!({"type":"status","session":"clock-session","generation":"clock-generation",
        "request_id":id,"scope":{"slot":slot,"target":{"kind":"application"}},
        "context":{"session":"clock-session"}})
}

struct Clock {
    child: Child,
    input: Option<ChildStdin>,
    output: Receiver<Value>,
    reader: Option<std::thread::JoinHandle<()>>,
}

impl Clock {
    fn start(binary: &str, timezone: Option<&OsStr>) -> Self {
        let mut command = Command::new(binary);
        command
            .env_remove("TZ")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(timezone) = timezone {
            command.env("TZ", timezone);
        }
        let mut child = command.spawn().unwrap();
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
                    .expect("clock stdout exceeded its bounded queue");
            }
        });
        Self {
            child,
            input: Some(input),
            output,
            reader: Some(reader),
        }
    }

    fn managed(timezone: Option<&OsStr>) -> Self {
        Self::start(env!("CARGO_BIN_EXE_clock-managed"), timezone)
    }

    fn send(&mut self, value: Value) {
        self.raw(format!("{value}\n").as_bytes());
    }

    fn raw(&mut self, bytes: &[u8]) {
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut remaining = bytes;
        while !remaining.is_empty() {
            assert!(Instant::now() < deadline, "clock stdin write deadline");
            match self.input.as_mut().unwrap().write(remaining) {
                Ok(0) => panic!("clock stdin closed before its frame was written"),
                Ok(count) => remaining = &remaining[count..],
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) =>
                {
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(error) => panic!("clock stdin write failed: {error}"),
            }
        }
    }

    fn receive(&self) -> Value {
        self.output
            .recv_timeout(Duration::from_secs(3))
            .expect("actual clock reply deadline")
    }

    fn admit(&mut self) {
        self.send(hello());
        assert_eq!(self.receive(), hello());
    }

    fn footer(&mut self, id: u64) -> Value {
        self.send(request(id, "app_footer"));
        let row = self.receive();
        assert_eq!(row["type"], "status");
        assert_eq!(row["request_id"], id);
        assert_eq!(row["session"], "clock-session");
        assert_eq!(row["generation"], "clock-generation");
        assert_eq!(row["refresh_ms"], 1000);
        assert_eq!(row["fragments"].as_array().unwrap().len(), 1);
        assert_eq!(row["fragments"][0]["tooltip"], "Local time");
        assert_eq!(row["fragments"][0]["priority"], 5);
        seconds(&row);
        row
    }

    fn exited(&mut self, success: bool) {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert_eq!(status.success(), success, "clock exited: {status}");
                break;
            }
            assert!(Instant::now() < deadline, "clock did not exit");
            std::thread::sleep(Duration::from_millis(2));
        }
        let reader = self.reader.take().unwrap();
        while !reader.is_finished() {
            assert!(
                Instant::now() < deadline,
                "clock stdout reader did not exit"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        reader.join().unwrap();
    }

    fn shutdown(&mut self) {
        self.send(json!({"type":"shutdown"}));
        // keep stdin open: a process that ignores shutdown cannot borrow EOF as
        // evidence that the protocol shutdown consumer actually executed.
        self.exited(true);
        self.input.take();
        assert!(self.output.try_recv().is_err());
    }
}

impl Drop for Clock {
    fn drop(&mut self) {
        self.input.take();
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn seconds(row: &Value) -> i64 {
    let text = row["fragments"][0]["text"].as_str().unwrap();
    assert_eq!(text.len(), 8, "clock format changed: {text}");
    chrono::NaiveTime::parse_from_str(text, "%H:%M:%S")
        .unwrap()
        .num_seconds_from_midnight()
        .into()
}

#[test]
fn managed_clock_admits_only_status_and_keeps_binding_correlation() {
    let mut clock = Clock::managed(Some(OsStr::new("UTC")));
    assert!(matches!(
        clock.output.recv_timeout(Duration::from_millis(30)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    let mut greeting = hello();
    greeting["features"] = json!(["status", "settings", "context.terminals", "context.cwd"]);
    clock.send(greeting);
    assert_eq!(clock.receive(), hello());
    clock.footer(1);

    // distinct stale IDs make a wrongly emitted reply observable even if it
    // arrives only after the following live request; no sleep proves rejection.
    for (field, stale_id, live_id) in [("session", 1000, 2), ("generation", 2000, 3)] {
        let mut stale = request(stale_id, "app_footer");
        stale[field] = json!("stale-binding");
        clock.send(stale);
        clock.footer(live_id);
    }
    clock.send(request(2, "app_footer"));
    clock.send(request(3, "app_footer"));
    clock.footer(4);
    clock.send(request(5, "app_header"));
    let empty = clock.receive();
    assert_eq!(empty["request_id"], 5);
    assert_eq!(empty["refresh_ms"], 1000);
    assert_eq!(empty["fragments"], json!([]));
    clock.shutdown();

    let mut eof = Clock::managed(None);
    eof.admit();
    eof.footer(1);
    eof.input.take();
    eof.exited(true);
    assert!(eof.output.try_recv().is_err());
}

#[test]
fn managed_clock_refuses_bad_admission_without_legacy_fallback() {
    let mut inputs = Vec::new();
    for (field, value) in [
        ("identity", json!("github:1431055733/not-clock")),
        ("major", json!(2)),
        ("minor", json!(0)),
        ("features", json!([])),
        ("features", json!(["status", "status"])),
    ] {
        let mut greeting = hello();
        greeting[field] = value;
        inputs.push(format!("{greeting}\n").into_bytes());
    }
    inputs.push(b"not JSON\n".to_vec());
    inputs.push(b"{\"type\":\"shutdown\",\"type\":\"managed_hello\"}\n".to_vec());
    inputs.push(b"{\"type\":\"hello\",\"major\":1,\"minor\":0}\n".to_vec());
    inputs.push(format!("{}\n", request(1, "app_footer")).into_bytes());
    // the reader rejects at the unterminated boundary itself. an extra byte
    // could race that correct exit and turn expected refusal into BrokenPipe.
    inputs.push(vec![b'x'; managed::MAX_FRAME_BYTES]);
    for input in inputs {
        let mut clock = Clock::managed(None);
        clock.raw(&input);
        clock.input.take();
        clock.exited(false);
        assert!(clock.output.try_recv().is_err());
    }
}

#[test]
fn managed_clock_never_uses_contextual_targets_or_ungranted_fields() {
    let mut inputs = Vec::new();
    for (slot, kind, reference) in [
        ("pane_header", "pane", "1"),
        ("pane_footer", "pane", "1"),
        ("workspace_header", "workspace", "10"),
        ("workspace_summary", "workspace", "10"),
    ] {
        let mut input = request(1, slot);
        input["scope"]["target"] = json!({"kind":kind,"reference":reference});
        inputs.push(input);
    }
    for (field, value) in [
        ("terminals", json!([])),
        ("focused_pane", json!("1")),
        ("active_workspace", Value::Null),
    ] {
        let mut input = request(1, "app_footer");
        input["context"][field] = value;
        inputs.push(input);
    }
    for input in inputs {
        let mut clock = Clock::managed(None);
        clock.admit();
        clock.send(input);
        clock.input.take();
        clock.exited(false);
        assert!(clock.output.try_recv().is_err());
    }
}

#[test]
fn managed_clock_uses_real_utc_tokyo_time_and_one_second_refresh() {
    for (zone, offset) in [("UTC", 0), ("Asia/Tokyo", 9 * 3600)] {
        let mut clock = Clock::managed(Some(OsStr::new(zone)));
        clock.admit();
        let mut observed = Vec::new();
        for id in 1..=2 {
            let before = chrono::Utc::now().timestamp();
            let row = clock.footer(id);
            let after = chrono::Utc::now().timestamp();
            assert!(
                (0..=5).contains(&(after - before)),
                "wall-clock fence changed"
            );
            let value = seconds(&row);
            assert!(
                (before..=after).any(|second| (second + offset).rem_euclid(86400) == value),
                "{zone}: actual clock value is outside its request-time fence: {row}"
            );
            observed.push(value);
            if id == 1 {
                std::thread::sleep(Duration::from_millis(1100));
            }
        }
        assert_ne!(observed[0], observed[1], "{zone}: clock did not advance");
        clock.shutdown();
    }
}

#[test]
fn managed_clock_handles_unset_empty_and_nonstandard_timezone_inputs() {
    let zones = [
        None,
        Some(OsString::new()),
        Some(OsString::from("UTC-2")),
        Some(OsString::from(":Asia/Tokyo")),
        Some(OsString::from(":/usr/share/zoneinfo/Asia/Tokyo")),
        Some(OsString::from("/usr/share/zoneinfo/UTC")),
        Some(OsString::from("unrecognized-timezone-expression")),
        Some(OsString::from_vec(vec![b'U', 0x80])),
    ];
    for zone in zones {
        let mut clock = Clock::managed(zone.as_deref());
        clock.admit();
        let start = Instant::now();
        let before = seconds(&clock.footer(1));
        let after = seconds(&clock.footer(2));
        assert!(start.elapsed() < Duration::from_secs(10));
        // Only the managed process is supported. UTC/Tokyo accuracy has its own
        // independent UTC-fence test; these inputs check bounded valid output,
        // not compatibility with a removed executable or every libc TZ syntax.
        assert!(
            (after - before).rem_euclid(86400) <= 10,
            "managed clock jumped unexpectedly: {zone:?}"
        );
        clock.shutdown();
    }
}
