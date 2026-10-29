use crate::Result;
use rustix::fs::{fcntl_getfl, fcntl_setfl, OFlags};
use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Owned(Child);

impl Drop for Owned {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

fn drain(reader: &mut impl Read, bytes: &mut Vec<u8>, maximum: usize) -> Result<bool> {
    let mut buffer = [0; 4096];
    match reader.read(&mut buffer) {
        Ok(0) => Ok(true),
        Ok(count) => {
            if bytes.len() + count > maximum {
                return Err("subprocess output limit exceeded".into());
            }
            bytes.extend_from_slice(&buffer[..count]);
            Ok(false)
        }
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
            ) =>
        {
            Ok(false)
        }
        Err(error) => Err(error.into()),
    }
}

// The deadline covers collection, not OS spawn/reap. Only the direct child is
// owned. Callers run fixed tools or, in packaging, the entrypoint just built
// from the packaging repository itself; never a shell command from a publisher.
pub fn capture(command: &mut Command, input: &[u8], maximum: usize) -> Result<Vec<u8>> {
    if input.len() > 4096 {
        return Err("subprocess input exceeds one pipe write".into());
    }
    let mut child = Owned(
        command
            .env("LC_ALL", "C")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?,
    );
    if let Some(mut stdin) = child.0.stdin.take() {
        stdin.write_all(input)?;
    }
    let mut stdout = child.0.stdout.take().ok_or("missing stdout")?;
    let mut stderr = child.0.stderr.take().ok_or("missing stderr")?;
    fcntl_setfl(&stdout, fcntl_getfl(&stdout)? | OFlags::NONBLOCK)?;
    fcntl_setfl(&stderr, fcntl_getfl(&stderr)? | OFlags::NONBLOCK)?;
    let deadline = Instant::now() + Duration::from_secs(30);
    let (mut output, mut errors) = (Vec::new(), Vec::new());
    let (mut out_closed, mut err_closed, mut status) = (false, false, None);
    loop {
        if Instant::now() >= deadline {
            return Err("subprocess collection deadline exceeded".into());
        }
        if !out_closed {
            out_closed = drain(&mut stdout, &mut output, maximum)?;
        }
        if !err_closed {
            err_closed = drain(&mut stderr, &mut errors, 64 * 1024)?;
        }
        if status.is_none() {
            status = child.0.try_wait()?;
        }
        if let Some(status) = status {
            if out_closed && err_closed {
                if !status.success() {
                    return Err(format!(
                        "subprocess failed ({status}): {}",
                        String::from_utf8_lossy(&errors)
                    )
                    .into());
                }
                return Ok(output);
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

pub fn text(command: &mut Command) -> Result<String> {
    Ok(String::from_utf8(capture(command, &[], 4 * 1024 * 1024)?)?)
}
