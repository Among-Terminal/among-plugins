use among_example_plugins::managed;
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

#[test]
fn cargo_exposes_only_the_three_managed_production_entrypoints() {
    let output = Command::new(env!("CARGO"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args([
            "metadata",
            "--offline",
            "--locked",
            "--no-deps",
            "--format-version=1",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output);
    let metadata: Value = serde_json::from_slice(&output.stdout).unwrap();
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
