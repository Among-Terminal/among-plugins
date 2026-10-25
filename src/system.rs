use crate::managed::{Scope, Target};
use crate::protocol::Status;
use serde_json::json;
use std::time::Duration;

#[derive(Clone, Copy, Debug)]
pub struct Cpu([u64; 8]);

pub fn parse_cpu(text: &str) -> Option<Cpu> {
    let mut row = text
        .lines()
        .find(|line| line.starts_with("cpu "))?
        .split_whitespace();
    row.next()?;
    let mut values = [0; 8];
    for value in &mut values {
        *value = row.next()?.parse().ok()?;
    }
    Some(Cpu(values))
}

fn percent(used: u64, total: u64, ceil: bool) -> Option<u8> {
    if total == 0 || used > total {
        return None;
    }
    let numerator = u128::from(used) * 100;
    let denominator = u128::from(total);
    let value = if ceil {
        numerator.div_ceil(denominator)
    } else {
        (numerator + denominator / 2) / denominator
    };
    u8::try_from(value).ok()
}

pub fn cpu_percent(previous: Cpu, current: Cpu) -> Option<u8> {
    let mut total = 0u64;
    let mut idle = 0u64;
    for (index, (before, after)) in previous.0.into_iter().zip(current.0).enumerate() {
        let delta = after.checked_sub(before)?;
        total = total.checked_add(delta)?;
        if index == 3 || index == 4 {
            idle = idle.checked_add(delta)?;
        }
    }
    percent(total.checked_sub(idle)?, total, false)
}

pub fn memory_percent(text: &str) -> Option<u8> {
    let value = |key: &str| -> Option<u64> {
        let line = text.lines().find(|line| line.starts_with(key))?;
        let mut fields = line.split_whitespace();
        if fields.next()? != key {
            return None;
        }
        let value = fields.next()?.parse().ok()?;
        if fields.next()? != "kB" {
            return None;
        }
        Some(value)
    };
    let total = value("MemTotal:")?;
    percent(total.checked_sub(value("MemAvailable:")?)?, total, false)
}

pub fn disk_percent(blocks: u64, free: u64, available: u64) -> Option<u8> {
    if available > free {
        return None;
    }
    let used = blocks.checked_sub(free)?;
    percent(used, used.checked_add(available)?, true)
}

/// Source injection is for deterministic counter/failure and read-rate tests.
pub trait Source {
    fn cpu(&mut self) -> Option<Cpu>;
    fn memory(&mut self) -> Option<u8>;
    fn disk(&mut self) -> Option<u8>;
}

pub struct Linux;
impl Source for Linux {
    fn cpu(&mut self) -> Option<Cpu> {
        parse_cpu(&std::fs::read_to_string("/proc/stat").ok()?)
    }
    fn memory(&mut self) -> Option<u8> {
        memory_percent(&std::fs::read_to_string("/proc/meminfo").ok()?)
    }
    fn disk(&mut self) -> Option<u8> {
        let stat = rustix::fs::statvfs("/").ok()?;
        disk_percent(stat.f_blocks, stat.f_bfree, stat.f_bavail)
    }
}

pub struct Sampler<S: Source> {
    pub source: S,
    previous_cpu: Option<Cpu>,
    last_cpu: Option<Duration>,
    last_disk: Option<Duration>,
    values: [Option<u8>; 3],
}
impl<S: Source> Sampler<S> {
    pub fn new(source: S) -> Self {
        Self {
            source,
            previous_cpu: None,
            last_cpu: None,
            last_disk: None,
            values: [None; 3],
        }
    }

    // admission stays in the managed transport; both explicit entrypoints use
    // the same sampler, counters and rate limits rather than a second renderer.
    pub fn managed_status(&mut self, scope: &Scope, now: Duration) -> Status {
        if scope.target != Target::Application {
            return Status::empty(Some(1000));
        }
        self.status(&scope.slot, now)
    }

    pub fn status(&mut self, scope: &str, now: Duration) -> Status {
        if scope != "app_header" {
            return Status::empty(Some(1000));
        }
        if self
            .last_cpu
            .is_none_or(|last| now.saturating_sub(last) >= Duration::from_secs(1))
        {
            let current = self.source.cpu();
            self.values[0] = self
                .previous_cpu
                .zip(current)
                .and_then(|(a, b)| cpu_percent(a, b));
            self.previous_cpu = current;
            self.values[1] = self.source.memory();
            self.last_cpu = Some(now);
        }
        if self
            .last_disk
            .is_none_or(|last| now.saturating_sub(last) >= Duration::from_secs(5))
        {
            self.values[2] = self.source.disk();
            self.last_disk = Some(now);
        }
        let labels = ["CPU", "MEM", "DISK"];
        let tips = [
            "Aggregate CPU busy time; idle and I/O wait excluded",
            "Used memory based on MemAvailable",
            "Root filesystem /; df-style used space excluding reserved availability",
        ];
        let fragments = labels.into_iter().zip(self.values).zip(tips).enumerate().map(|(index,((label,value),tip))| {
                json!({"text":meter(label,value),"meter":value.map(|v|f64::from(v)/100.0),"tooltip":tip,"priority":10+index as i32})
        }).collect();
        Status {
            refresh_ms: Some(1000),
            fragments,
        }
    }
}

pub fn meter(label: &str, value: Option<u8>) -> String {
    match value {
        Some(value) => {
            let value = value.min(100);
            format!("{label} {value}%")
        }
        None => format!("{label} N/A"),
    }
}
