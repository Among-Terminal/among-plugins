use among_example_plugins::managed;
use rustix::fs::{fcntl_getfl, fcntl_setfl, OFlags};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Process(Child);

impl Drop for Process {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

fn collect_available(reader: &mut impl Read, bytes: &mut Vec<u8>, limit: usize) -> bool {
    let mut buffer = [0; 4096];
    match reader.read(&mut buffer) {
        Ok(0) => true,
        Ok(count) => {
            assert!(bytes.len() + count <= limit, "Cargo metadata output limit");
            bytes.extend_from_slice(&buffer[..count]);
            false
        }
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
            ) =>
        {
            false
        }
        Err(error) => panic!("Cargo metadata pipe failed: {error}"),
    }
}

#[test]
fn cargo_exposes_only_the_three_managed_production_entrypoints() {
    let mut process = Process(
        Command::new(env!("CARGO"))
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .args([
                "metadata",
                "--offline",
                "--locked",
                "--no-deps",
                "--format-version=1",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let mut stdout = process.0.stdout.take().unwrap();
    let mut stderr = process.0.stderr.take().unwrap();
    fcntl_setfl(&stdout, fcntl_getfl(&stdout).unwrap() | OFlags::NONBLOCK).unwrap();
    fcntl_setfl(&stderr, fcntl_getfl(&stderr).unwrap() | OFlags::NONBLOCK).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut output = Vec::new();
    let mut errors = Vec::new();
    let mut status = None;
    let mut stdout_closed = false;
    let mut stderr_closed = false;
    loop {
        assert!(Instant::now() < deadline, "Cargo metadata deadline");
        if !stdout_closed {
            stdout_closed = collect_available(&mut stdout, &mut output, 1024 * 1024);
        }
        if !stderr_closed {
            stderr_closed = collect_available(&mut stderr, &mut errors, 16 * 1024);
        }
        if status.is_none() {
            status = process.0.try_wait().unwrap();
        }
        if status.is_some() && stdout_closed && stderr_closed {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(
        status.unwrap().success(),
        "Cargo metadata failed: {}",
        String::from_utf8_lossy(&errors)
    );
    let metadata: Value = serde_json::from_slice(&output).unwrap();
    let packages = metadata["packages"].as_array().unwrap();
    assert_eq!(packages.len(), 1);
    let mut binaries = packages[0]["targets"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|target| target["kind"] == json!(["bin"]))
        .map(|target| target["name"].as_str().unwrap())
        .collect::<Vec<_>>();
    binaries.sort_unstable();
    assert_eq!(
        binaries,
        ["clock-managed", "pane-pwd-managed", "system-status-managed"]
    );
    assert!(packages[0]["targets"]
        .as_array()
        .unwrap()
        .iter()
        .all(|target| target["name"] != "install"));
}

#[test]
fn every_production_process_refuses_legacy_input_without_waiting_for_eof() {
    for binary in [
        env!("CARGO_BIN_EXE_clock-managed"),
        env!("CARGO_BIN_EXE_system-status-managed"),
        env!("CARGO_BIN_EXE_pane-pwd-managed"),
    ] {
        for message in [
            json!({"type":"hello","major":1,"minor":0}),
            json!({"type":"hello","major":1,"minor":99}),
            json!({"type":"status","request_id":1,"scope":"app_header","context":{}}),
        ] {
            let mut process = Process(
                Command::new(binary)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .spawn()
                    .unwrap(),
            );
            let input = process.0.stdin.as_mut().unwrap();
            writeln!(input, "{message}").unwrap();
            input.flush().unwrap();
            let deadline = Instant::now() + Duration::from_secs(3);
            let status = loop {
                if let Some(status) = process.0.try_wait().unwrap() {
                    break status;
                }
                assert!(
                    Instant::now() < deadline,
                    "legacy input did not exit: {binary}"
                );
                std::thread::sleep(Duration::from_millis(2));
            };
            assert!(!status.success(), "legacy input was accepted: {binary}");
            // stdin stays open until after the observed exit, so EOF cannot be
            // mistaken for the unsupported-protocol refusal.
            assert!(process.0.stdin.is_some());
            let mut output = Vec::new();
            process
                .0
                .stdout
                .take()
                .unwrap()
                .take((managed::MAX_FRAME_BYTES + 1) as u64)
                .read_to_end(&mut output)
                .unwrap();
            assert!(
                output.is_empty(),
                "unsupported input produced status: {binary}"
            );
        }
    }
}
