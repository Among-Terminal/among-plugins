use among_example_plugins::{managed, system};
use rustix::fs::{fcntl_getfl, fcntl_setfl, OFlags};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

const LABELS: [&str; 3] = ["CPU", "MEM", "DISK"];
const TIPS: [&str; 3] = [
    "Aggregate CPU busy time; idle and I/O wait excluded",
    "Used memory based on MemAvailable",
    "Root filesystem /; df-style used space excluding reserved availability",
];

fn hello() -> Value {
    json!({"type":"managed_hello","major":1,"minor":2,
        "identity":"github:1431055733/system-status","session":"resource-session",
        "generation":"resource-generation","features":["status"]})
}

fn request(id: u64, slot: &str) -> Value {
    json!({"type":"status","session":"resource-session","generation":"resource-generation",
        "request_id":id,"scope":{"slot":slot,"target":{"kind":"application"}},
        "context":{"session":"resource-session"}})
}

struct Owned(Child);

impl Drop for Owned {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

struct Process {
    child: Owned,
    input: Option<ChildStdin>,
    output: Receiver<Value>,
    reader: Option<std::thread::JoinHandle<()>>,
}

impl Process {
    fn start() -> Self {
        let mut child = Owned(
            Command::new(env!("CARGO_BIN_EXE_system-status-managed"))
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let input = child.0.stdin.take().unwrap();
        let flags = fcntl_getfl(&input).unwrap();
        fcntl_setfl(&input, flags | OFlags::NONBLOCK).unwrap();
        let stdout = child.0.stdout.take().unwrap();
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
                    .expect("resource stdout exceeded its bounded queue");
            }
        });
        Self {
            child,
            input: Some(input),
            output,
            reader: Some(reader),
        }
    }

    fn raw(&mut self, bytes: &[u8]) {
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut remaining = bytes;
        while !remaining.is_empty() {
            assert!(Instant::now() < deadline, "resource stdin write deadline");
            match self.input.as_mut().unwrap().write(remaining) {
                Ok(0) => panic!("resource stdin closed before its frame was written"),
                Ok(count) => remaining = &remaining[count..],
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) =>
                {
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(error) => panic!("resource stdin write failed: {error}"),
            }
        }
    }

    fn send(&mut self, value: Value) {
        self.raw(format!("{value}\n").as_bytes());
    }

    fn receive(&self) -> Value {
        self.output
            .recv_timeout(Duration::from_secs(3))
            .expect("actual resource reply deadline")
    }

    fn admit(&mut self) {
        self.send(hello());
        assert_eq!(self.receive(), hello());
    }

    fn header(&mut self, id: u64) -> Value {
        self.send(request(id, "app_header"));
        let row = self.receive();
        assert_eq!(row["type"], "status");
        assert_eq!(row["session"], "resource-session");
        assert_eq!(row["generation"], "resource-generation");
        assert_eq!(row["request_id"], id);
        assert_eq!(row["refresh_ms"], 1000);
        assert_eq!(row["fragments"].as_array().unwrap().len(), 3);
        for (index, label) in LABELS.iter().enumerate() {
            let fragment = &row["fragments"][index];
            assert_eq!(fragment["tooltip"], TIPS[index]);
            assert_eq!(fragment["priority"], 10 + index);
            if fragment["text"] == format!("{label} N/A") {
                assert!(fragment["meter"].is_null());
            } else {
                percentage(&row, index);
            }
        }
        row
    }

    fn exited(&mut self, success: bool) {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(status) = self.child.0.try_wait().unwrap() {
                assert_eq!(status.success(), success, "resource exited: {status}");
                break;
            }
            assert!(Instant::now() < deadline, "resource did not exit");
            std::thread::sleep(Duration::from_millis(2));
        }
        let reader = self.reader.take().unwrap();
        while !reader.is_finished() {
            assert!(Instant::now() < deadline, "resource reader did not exit");
            std::thread::sleep(Duration::from_millis(2));
        }
        reader.join().unwrap();
    }

    fn shutdown(&mut self) {
        self.send(json!({"type":"shutdown"}));
        // ignoring shutdown cannot pass by borrowing stdin EOF.
        self.exited(true);
        self.input.take();
        assert!(self.output.try_recv().is_err());
    }
}

fn percentage(row: &Value, index: usize) -> u8 {
    let fragment = &row["fragments"][index];
    let number = fragment["text"]
        .as_str()
        .unwrap()
        .strip_prefix(&format!("{} ", LABELS[index]))
        .unwrap()
        .strip_suffix('%')
        .unwrap()
        .parse::<u8>()
        .unwrap();
    assert!(number <= 100);
    assert_eq!(fragment["meter"].as_f64(), Some(f64::from(number) / 100.0));
    number
}

#[test]
fn managed_system_admits_only_status_preserves_binding_and_stops() {
    let mut process = Process::start();
    assert!(matches!(
        process.output.recv_timeout(Duration::from_millis(30)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    let mut greeting = hello();
    greeting["features"] = json!(["status", "settings", "context.terminals", "context.cwd"]);
    process.send(greeting);
    assert_eq!(process.receive(), hello());
    let first = process.header(1);
    assert_eq!(first["fragments"][0]["text"], "CPU N/A");
    assert!(first["fragments"][0]["meter"].is_null());
    for (field, stale_id, live_id) in [("session", 1000, 2), ("generation", 2000, 3)] {
        let mut stale = request(stale_id, "app_header");
        stale[field] = json!("stale-binding");
        process.send(stale);
        process.header(live_id);
    }
    process.send(request(2, "app_header"));
    process.send(request(3, "app_header"));
    process.header(4);
    process.send(request(5, "app_footer"));
    let empty = process.receive();
    assert_eq!(empty["request_id"], 5);
    assert_eq!(empty["refresh_ms"], 1000);
    assert_eq!(empty["fragments"], json!([]));
    process.shutdown();

    let mut eof = Process::start();
    eof.admit();
    eof.header(1);
    eof.input.take();
    eof.exited(true);
    assert!(eof.output.try_recv().is_err());
}

#[test]
fn managed_system_refuses_bad_admission_and_ungranted_targets() {
    let mut inputs = Vec::new();
    for (field, value) in [
        ("identity", json!("github:1431055733/not-system-status")),
        ("major", json!(2)),
        ("minor", json!(0)),
        ("features", json!([])),
        ("features", json!(["status", "status"])),
    ] {
        let mut greeting = hello();
        greeting[field] = value;
        inputs.push(format!("{greeting}\n").into_bytes());
    }
    let greeting = hello().to_string();
    inputs.push(
        format!(
            "{{\"type\":\"managed_hello\",{}\n",
            greeting.strip_prefix('{').unwrap()
        )
        .into_bytes(),
    );
    inputs.push(b"not JSON\n".to_vec());
    inputs.push(b"{\"type\":\"hello\",\"major\":1,\"minor\":0}\n".to_vec());
    inputs.push(format!("{}\n", request(1, "app_header")).into_bytes());
    // rejection happens at this exact unterminated boundary, not after a byte
    // whose write could race the child's already-correct refusal.
    inputs.push(vec![b'x'; managed::MAX_FRAME_BYTES]);
    for input in inputs {
        let mut process = Process::start();
        process.raw(&input);
        process.exited(false);
        assert!(process.output.try_recv().is_err());
    }

    let mut inputs = Vec::new();
    for (slot, kind) in [
        ("pane_header", "pane"),
        ("pane_footer", "pane"),
        ("workspace_header", "workspace"),
        ("workspace_summary", "workspace"),
    ] {
        let mut input = request(1, slot);
        input["scope"]["target"] = json!({"kind":kind,"reference":"1"});
        inputs.push(input);
    }
    for (field, value) in [
        ("terminals", json!([])),
        ("focused_pane", json!("1")),
        ("active_workspace", Value::Null),
    ] {
        let mut input = request(1, "app_header");
        input["context"][field] = value;
        inputs.push(input);
    }
    for input in inputs {
        let mut process = Process::start();
        process.admit();
        process.send(input);
        process.exited(false);
        assert!(process.output.try_recv().is_err());
    }
}

fn reference_cpu() -> [u64; 8] {
    // independent of the production parser and its Cpu representation.
    let text = std::fs::read_to_string("/proc/stat").unwrap();
    let row = text.lines().find(|line| line.starts_with("cpu ")).unwrap();
    row.split_whitespace()
        .skip(1)
        .take(8)
        .map(|field| field.parse::<u64>().unwrap())
        .collect::<Vec<_>>()
        .try_into()
        .unwrap()
}

fn reference_cpu_delta(before: [u64; 8], after: [u64; 8]) -> (u64, u64) {
    let differences = before
        .into_iter()
        .zip(after)
        .map(|(start, end)| {
            end.checked_sub(start)
                .expect("independent CPU counter moved backwards; live interval invalid")
        })
        .collect::<Vec<_>>();
    let total: u64 = differences.iter().sum();
    assert!(total > 0);
    (total - differences[3] - differences[4], total)
}

fn reference_cpu_range(before: [[u64; 8]; 2], after: [[u64; 8]; 2]) -> [f64; 2] {
    // the actual two sampler reads fall inside these independent brackets.
    // bound numerator and denominator separately instead of assuming the 1100ms
    // sleep made our observation interval identical to the child's interval.
    let (busy_low, total_low) = reference_cpu_delta(before[1], after[0]);
    let (busy_high, total_high) = reference_cpu_delta(before[0], after[1]);
    let low = 100.0 * busy_low as f64 / total_high as f64;
    let high = (100.0 * busy_high as f64 / total_low as f64).min(100.0);
    assert!(
        low <= high && high - low <= 2.0,
        "CPU reference skew too large: [{low}, {high}]"
    );
    [low, high]
}

fn reference_memory() -> f64 {
    let text = std::fs::read_to_string("/proc/meminfo").unwrap();
    let read = |name: &str| {
        let row = text
            .lines()
            .find(|line| line.split_whitespace().next() == Some(name))
            .unwrap();
        let fields = row.split_whitespace().collect::<Vec<_>>();
        assert_eq!(fields.len(), 3);
        assert_eq!(fields[2], "kB");
        fields[1].parse::<u64>().unwrap()
    };
    let total = read("MemTotal:");
    let available = read("MemAvailable:");
    assert!(total > 0 && available <= total);
    100.0 * (total - available) as f64 / total as f64
}

fn reference_disk() -> f64 {
    let mut child = Owned(
        Command::new("/usr/bin/df")
            .args(["-P", "/"])
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let mut stdout = child.0.stdout.take().unwrap();
    let flags = fcntl_getfl(&stdout).unwrap();
    fcntl_setfl(&stdout, flags | OFlags::NONBLOCK).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut output = Vec::new();
    loop {
        assert!(Instant::now() < deadline, "independent df read deadline");
        let mut bytes = [0; 512];
        match stdout.read(&mut bytes) {
            Ok(0) => break,
            Ok(count) => {
                output.extend_from_slice(&bytes[..count]);
                assert!(output.len() <= 4096);
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ) =>
            {
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(error) => panic!("independent df read failed: {error}"),
        }
    }
    loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            assert!(status.success(), "independent df failed: {status}");
            break;
        }
        assert!(Instant::now() < deadline, "independent df exit deadline");
        std::thread::sleep(Duration::from_millis(2));
    }
    let text = String::from_utf8(output).unwrap();
    let fields = text
        .lines()
        .last()
        .unwrap()
        .split_whitespace()
        .collect::<Vec<_>>();
    assert!(fields.len() >= 6);
    assert_eq!(fields.last(), Some(&"/"));
    fields[fields.len() - 2]
        .strip_suffix('%')
        .unwrap()
        .parse::<f64>()
        .unwrap()
}

#[test]
fn managed_system_measures_real_cpu_memory_and_root_disk() {
    let mut process = Process::start();
    process.admit();
    let disk_started = Instant::now();
    let disk_before = reference_disk();
    let cpu_before = reference_cpu();
    let started = Instant::now();
    let first = process.header(1);
    let cpu_after = reference_cpu();
    let first_skew = started.elapsed();
    assert!(
        first_skew <= Duration::from_millis(200),
        "first CPU observation skew: {first_skew:?}"
    );
    let disk_after = reference_disk();
    let disk_window = disk_started.elapsed();
    assert!(
        disk_window <= Duration::from_secs(1),
        "initial disk reference skew: {disk_window:?}"
    );
    assert_eq!(first["fragments"][0]["text"], "CPU N/A");
    assert!(first["fragments"][0]["meter"].is_null());
    percentage(&first, 1);
    let initial_disk = percentage(&first, 2);
    assert!(
        (disk_before - disk_after).abs() <= 1.0,
        "root filesystem changed during initial disk sample"
    );
    let independent_disk = (disk_before + disk_after) / 2.0;
    assert!((f64::from(initial_disk) - independent_disk).abs() <= 1.0);
    std::thread::sleep(Duration::from_millis(1100));
    let memory_before = reference_memory();
    let next_cpu_before = reference_cpu();
    let second_started = Instant::now();
    let measured = process.header(2);
    let next_cpu_after = reference_cpu();
    let memory_after = reference_memory();
    let second_skew = second_started.elapsed();
    assert!(
        second_skew <= Duration::from_millis(200),
        "second CPU observation skew: {second_skew:?}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "observation no longer fits the disk cache interval"
    );
    let cpu_range = reference_cpu_range([cpu_before, cpu_after], [next_cpu_before, next_cpu_after]);
    assert!(
        (memory_before - memory_after).abs() <= 2.0,
        "memory reference skew too large"
    );
    let independent = [
        (cpu_range[0] + cpu_range[1]) / 2.0,
        (memory_before + memory_after) / 2.0,
        independent_disk,
    ];
    let actual = std::array::from_fn::<_, 3, _>(|index| percentage(&measured, index));
    // disk is legitimately cached for five seconds. compare its initial sample
    // to nearby df output, not to filesystem usage after an unrelated delay.
    assert_eq!(actual[2], initial_disk);
    for (index, tolerance) in [2.0, 2.0, 1.0].into_iter().enumerate() {
        assert!(independent[index].is_finite());
        assert!(
            (f64::from(actual[index]) - independent[index]).abs() <= tolerance,
            "{} differs: actual={} independent={} tolerance={tolerance}",
            LABELS[index],
            actual[index],
            independent[index]
        );
    }
    println!(
        "actual_managed_resources interval_seconds={} plugin_percent={actual:?} independent_percent={independent:?} tolerances=[2,2,1] cpu_reference_range={cpu_range:?} cpu_skew_seconds=[{},{}] disk_reference_window_seconds={} root_mount=/ proc_view=inherited",
        started.elapsed().as_secs_f64(), first_skew.as_secs_f64(), second_skew.as_secs_f64(), disk_window.as_secs_f64()
    );
    process.shutdown();
}

struct Probe {
    cpu: Option<system::Cpu>,
    memory: Option<u8>,
    disk: Option<u8>,
    reads: [u64; 3],
}

impl system::Source for Probe {
    fn cpu(&mut self) -> Option<system::Cpu> {
        self.reads[0] += 1;
        self.cpu
    }
    fn memory(&mut self) -> Option<u8> {
        self.reads[1] += 1;
        self.memory
    }
    fn disk(&mut self) -> Option<u8> {
        self.reads[2] += 1;
        self.disk
    }
}

fn counter(busy: u64, idle: u64) -> Option<system::Cpu> {
    system::parse_cpu(&format!("cpu {busy} 0 0 {idle} 0 0 0 0"))
}

fn probe() -> Probe {
    Probe {
        cpu: counter(0, 0),
        memory: Some(25),
        disk: Some(78),
        reads: [0; 3],
    }
}

fn scope(slot: &str, kind: &str) -> managed::Scope {
    let target = if kind == "application" {
        json!({"kind":kind})
    } else {
        json!({"kind":kind,"reference":"1"})
    };
    serde_json::from_value(json!({"slot":slot,"target":target})).unwrap()
}

#[test]
fn managed_system_sampler_preserves_rates_and_ignores_other_slots() {
    let mut sampler = system::Sampler::new(probe());
    for (slot, kind) in [
        ("app_footer", "application"),
        ("unsupported_slot", "application"),
        // app_header would sample without the adapter's target guard. these
        // direct calls test that defense, not the transport's scope admission.
        ("app_header", "pane"),
        ("app_header", "workspace"),
        ("pane_header", "pane"),
        ("pane_footer", "pane"),
        ("workspace_header", "workspace"),
        ("workspace_summary", "workspace"),
    ] {
        let empty = sampler.managed_status(&scope(slot, kind), Duration::from_secs(50));
        assert!(empty.fragments.is_empty());
        assert_eq!(empty.refresh_ms, Some(1000));
    }
    assert_eq!(sampler.source.reads, [0, 0, 0]);
    let header = scope("app_header", "application");
    let first = sampler.managed_status(&header, Duration::ZERO);
    assert_eq!(first.fragments[0]["text"], "CPU N/A");
    for _ in 0..100 {
        sampler.managed_status(&header, Duration::from_millis(999));
    }
    assert_eq!(sampler.source.reads, [1, 1, 1]);
    sampler.source.cpu = counter(50, 50);
    let second = sampler.managed_status(&header, Duration::from_secs(1));
    assert_eq!(second.fragments[0]["text"], "CPU 50%");
    assert_eq!(second.fragments[0]["meter"], 0.5);
    assert_eq!(sampler.source.reads, [2, 2, 1]);
    sampler.source.cpu = counter(100, 100);
    sampler.managed_status(&header, Duration::from_secs(5));
    assert_eq!(sampler.source.reads, [3, 3, 2]);
}

#[test]
fn managed_system_sampler_reports_each_failed_source_independently() {
    let header = scope("app_header", "application");
    for failed in 0..3 {
        let mut sampler = system::Sampler::new(probe());
        sampler.managed_status(&header, Duration::ZERO);
        sampler.source.cpu = counter(50, 50);
        let baseline = sampler.managed_status(&header, Duration::from_secs(1));
        for (index, label) in LABELS.iter().enumerate() {
            let value = [50, 25, 78][index];
            assert_eq!(
                baseline.fragments[index]["text"],
                format!("{label} {value}%")
            );
            assert_eq!(baseline.fragments[index]["meter"], f64::from(value) / 100.0);
        }
        sampler.source.cpu = (failed != 0).then(|| counter(100, 100).unwrap());
        sampler.source.memory = (failed != 1).then_some(25);
        sampler.source.disk = (failed != 2).then_some(78);
        let measured = sampler.managed_status(&header, Duration::from_secs(5));
        assert_eq!(measured.refresh_ms, Some(1000));
        assert_eq!(sampler.source.reads, [3, 3, 2]);
        for (index, label) in LABELS.iter().enumerate() {
            let fragment = &measured.fragments[index];
            if index == failed {
                assert_eq!(fragment["text"], format!("{label} N/A"));
                assert!(fragment["meter"].is_null());
            } else {
                let value = [50, 25, 78][index];
                assert_eq!(fragment["text"], format!("{label} {value}%"));
                assert_eq!(fragment["meter"], f64::from(value) / 100.0);
            }
        }
        sampler.source.cpu = counter(150, 150);
        sampler.source.memory = Some(25);
        sampler.source.disk = Some(78);
        let recovering = sampler.managed_status(&header, Duration::from_secs(6));
        assert_eq!(sampler.source.reads, [4, 4, 2]);
        if failed == 0 {
            assert_eq!(recovering.fragments[0]["text"], "CPU N/A");
            assert!(recovering.fragments[0]["meter"].is_null());
        } else {
            assert_eq!(recovering.fragments[0]["text"], "CPU 50%");
        }
        assert_eq!(recovering.fragments[1]["text"], "MEM 25%");
        if failed == 2 {
            assert_eq!(recovering.fragments[2]["text"], "DISK N/A");
            assert!(recovering.fragments[2]["meter"].is_null());
        }
        sampler.source.cpu = counter(200, 200);
        let recovered = sampler.managed_status(&header, Duration::from_secs(10));
        assert_eq!(sampler.source.reads, [5, 5, 3]);
        for (index, label) in LABELS.iter().enumerate() {
            let value = [50, 25, 78][index];
            assert_eq!(
                recovered.fragments[index]["text"],
                format!("{label} {value}%")
            );
            assert_eq!(
                recovered.fragments[index]["meter"],
                f64::from(value) / 100.0
            );
        }
    }
}
