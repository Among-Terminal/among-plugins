use rustix::event::{poll, PollFd, PollFlags, Timespec};
use rustix::fs::{fcntl_getfl, fcntl_setfl, OFlags};
use std::cell::Cell;
use std::io::{self, BufRead, Read, Write};
use std::os::fd::AsFd;
use std::rc::Rc;
use std::time::Instant;

pub(super) type Deadline = Rc<Cell<Option<Instant>>>;

pub(super) fn check(deadline: &Deadline) -> io::Result<()> {
    if deadline.get().is_some_and(|limit| Instant::now() >= limit) {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "managed traversal expired",
        ));
    }
    Ok(())
}

fn wait(fd: &impl AsFd, events: PollFlags, deadline: &Deadline) -> io::Result<()> {
    loop {
        check(deadline)?;
        let timeout = deadline
            .get()
            .map(|limit| {
                Timespec::try_from(limit.saturating_duration_since(Instant::now()))
                    .map_err(|_| super::wire::invalid())
            })
            .transpose()?;
        let mut fds = [PollFd::new(fd, events)];
        match poll(&mut fds, timeout.as_ref()) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "managed traversal expired",
                ))
            }
            Ok(_) if fds[0].revents().contains(PollFlags::NVAL) => {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "managed stdio unavailable",
                ));
            }
            Ok(_) => return check(deadline),
            Err(rustix::io::Errno::INTR) => continue,
            Err(error) => return Err(error.into()),
        }
    }
}

pub(super) struct Input {
    fd: io::Stdin,
    original: OFlags,
    deadline: Deadline,
    bytes: [u8; 4096],
    start: usize,
    end: usize,
}

impl Input {
    pub(super) fn new(deadline: Deadline) -> io::Result<Self> {
        let fd = io::stdin();
        let original = fcntl_getfl(&fd)?;
        fcntl_setfl(&fd, original | OFlags::NONBLOCK)?;
        Ok(Self {
            fd,
            original,
            deadline,
            bytes: [0; 4096],
            start: 0,
            end: 0,
        })
    }
}

impl BufRead for Input {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        check(&self.deadline)?;
        if self.start == self.end {
            self.start = 0;
            loop {
                check(&self.deadline)?;
                match rustix::io::read(&self.fd, &mut self.bytes) {
                    Ok(count) => {
                        self.end = count;
                        break;
                    }
                    Err(rustix::io::Errno::INTR) => continue,
                    Err(rustix::io::Errno::AGAIN) => wait(&self.fd, PollFlags::IN, &self.deadline)?,
                    Err(error) => return Err(error.into()),
                }
            }
        }
        Ok(&self.bytes[self.start..self.end])
    }

    fn consume(&mut self, amount: usize) {
        self.start += amount.min(self.end - self.start);
    }
}

impl Read for Input {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        let bytes = self.fill_buf()?;
        let count = bytes.len().min(output.len());
        output[..count].copy_from_slice(&bytes[..count]);
        self.consume(count);
        Ok(count)
    }
}

impl Drop for Input {
    fn drop(&mut self) {
        let _ = fcntl_setfl(&self.fd, self.original);
    }
}

pub(super) struct Output {
    fd: io::Stdout,
    original: OFlags,
    deadline: Deadline,
}

impl Output {
    pub(super) fn new(deadline: Deadline) -> io::Result<Self> {
        let fd = io::stdout();
        let original = fcntl_getfl(&fd)?;
        fcntl_setfl(&fd, original | OFlags::NONBLOCK)?;
        Ok(Self {
            fd,
            original,
            deadline,
        })
    }
}

impl Write for Output {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        loop {
            check(&self.deadline)?;
            match rustix::io::write(&self.fd, bytes) {
                Ok(count) => return Ok(count),
                Err(rustix::io::Errno::INTR) => continue,
                Err(rustix::io::Errno::AGAIN) => wait(&self.fd, PollFlags::OUT, &self.deadline)?,
                Err(error) => return Err(error.into()),
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        check(&self.deadline)
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        let _ = fcntl_setfl(&self.fd, self.original);
    }
}

pub(super) fn diagnose(kind: io::ErrorKind) {
    let fd = io::stderr();
    if let Ok(original) = fcntl_getfl(&fd) {
        if fcntl_setfl(&fd, original | OFlags::NONBLOCK).is_ok() {
            let message = format!("managed transport failed: {kind:?}\n");
            if message.len() <= 128 {
                let _ = rustix::io::write(&fd, message.as_bytes());
            }
            let _ = fcntl_setfl(&fd, original);
        }
    }
}
