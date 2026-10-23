use among_example_plugins::system::*;
use std::time::Duration;

fn cpu(values: &str) -> Cpu {
    parse_cpu(&format!("cpu {values}\n")).unwrap()
}

#[test]
fn cpu_intervals_exclude_guest_and_treat_iowait_as_idle() {
    let zero = cpu("0 0 0 0 0 0 0 0");
    assert_eq!(
        cpu_percent(zero, cpu("10 0 10 50 30 0 0 0 999 999")),
        Some(20)
    );
    assert_eq!(cpu_percent(zero, cpu("0 0 0 50 50 0 0 0")), Some(0));
    assert_eq!(cpu_percent(zero, cpu("100 0 0 0 0 0 0 0")), Some(100));
    assert_eq!(cpu_percent(zero, zero), None);
    assert_eq!(cpu_percent(cpu("1 0 0 0 0 0 0 0"), zero), None);
    assert!(parse_cpu("cpu 1 2 3").is_none());
    assert!(parse_cpu("cpu x 0 0 0 0 0 0 0").is_none());
    assert_eq!(
        cpu_percent(zero, cpu("18446744073709551615 1 0 0 0 0 0 0")),
        None
    );
}

#[test]
fn memory_disk_and_meter_boundaries_are_truthful() {
    assert_eq!(
        memory_percent("MemTotal: 1000 kB\nMemAvailable: 750 kB\nCached: 200 kB"),
        Some(25)
    );
    for text in [
        "",
        "MemTotal: 0 kB\nMemAvailable: 0 kB",
        "MemTotal: 1 kB\nMemAvailable: 2 kB",
        "MemTotal: 1 bytes\nMemAvailable: 0 bytes",
    ] {
        assert_eq!(memory_percent(text), None);
    }
    assert_eq!(disk_percent(100, 30, 20), Some(78));
    assert_eq!(disk_percent(100, 100, 100), Some(0));
    assert_eq!(disk_percent(0, 0, 0), None);
    assert_eq!(disk_percent(100, 101, 0), None);
    assert_eq!(disk_percent(100, 20, 30), None);
    assert_eq!(meter("CPU", Some(0)), "CPU 0%");
    assert_eq!(meter("CPU", Some(50)), "CPU 50%");
    assert_eq!(meter("CPU", Some(100)), "CPU 100%");
    assert_eq!(meter("CPU", None), "CPU N/A");
}

#[derive(Default)]
struct Probe {
    reads: [u64; 3],
    fail: bool,
}
impl Source for Probe {
    fn cpu(&mut self) -> Option<Cpu> {
        self.reads[0] += 1;
        (!self.fail).then(|| {
            cpu(&format!(
                "{} 0 0 {} 0 0 0 0",
                self.reads[0] * 10,
                self.reads[0] * 10
            ))
        })
    }
    fn memory(&mut self) -> Option<u8> {
        self.reads[1] += 1;
        (!self.fail).then_some(25)
    }
    fn disk(&mut self) -> Option<u8> {
        self.reads[2] += 1;
        (!self.fail).then_some(78)
    }
}

#[test]
fn redundant_scopes_do_not_multiply_reads_and_failure_is_per_source() {
    let mut sampler = Sampler::new(Probe::default());
    assert!(sampler
        .status("pane_header:1", Duration::ZERO)
        .fragments
        .is_empty());
    assert_eq!(sampler.source.reads, [0, 0, 0]);
    assert_eq!(
        sampler.status("app_header", Duration::ZERO).fragments[0]["text"],
        "CPU N/A"
    );
    for _ in 0..100 {
        sampler.status("app_header", Duration::from_millis(999));
    }
    assert_eq!(sampler.source.reads, [1, 1, 1]);
    let second = sampler.status("app_header", Duration::from_secs(1));
    assert_eq!(second.fragments[0]["text"], "CPU 50%");
    assert_eq!(second.fragments[0]["meter"], 0.5);
    sampler.source.fail = true;
    let failed = sampler.status("app_header", Duration::from_secs(2));
    assert_eq!(failed.fragments[0]["text"], "CPU N/A");
    assert_eq!(failed.fragments[1]["text"], "MEM N/A");
    assert_eq!(failed.fragments[2]["text"], "DISK 78%");
    let failed = sampler.status("app_header", Duration::from_secs(5));
    assert_eq!(failed.fragments[2]["text"], "DISK N/A");
    assert_eq!(sampler.source.reads, [4, 4, 2]);
}
