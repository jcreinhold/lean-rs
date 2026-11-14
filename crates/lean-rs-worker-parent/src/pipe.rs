//! Interruptible nonblocking Unix pipes. The child retains blocking descriptors;
//! only the parent's separate pipe ends change mode.
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::session::LeanWorkerCancellationToken;

#[derive(Debug)]
pub(crate) struct Pipe<T> {
    inner: T,
    pub(crate) stopped: Arc<AtomicBool>,
    pub(crate) deadline: Option<Instant>,
    pub(crate) cancellation: Option<LeanWorkerCancellationToken>,
}

impl<T: AsRawFd> Pipe<T> {
    #[allow(
        unsafe_code,
        reason = "fcntl changes nonblocking mode on an owned Unix pipe descriptor"
    )]
    pub(crate) fn new(inner: T) -> io::Result<Self> {
        // SAFETY: inner owns this valid descriptor throughout both fcntl calls.
        // F_GETFL takes no argument; F_SETFL receives the flags, not a pointer.
        let flags = unsafe { libc::fcntl(inner.as_raw_fd(), libc::F_GETFL) };
        if flags < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: same owned descriptor; preserve all existing file status flags.
        if unsafe { libc::fcntl(inner.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            inner,
            stopped: Arc::new(AtomicBool::new(false)),
            deadline: None,
            cancellation: None,
        })
    }
}

impl<T> Pipe<T> {
    fn check(&self) -> io::Result<()> {
        if self.stopped.load(Ordering::Acquire)
            || self
                .cancellation
                .as_ref()
                .is_some_and(LeanWorkerCancellationToken::is_cancelled)
            || self.deadline.is_some_and(|deadline| Instant::now() >= deadline)
        {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "pipe operation stopped or expired",
            ));
        }
        Ok(())
    }

    fn retry<R>(&mut self, mut operation: impl FnMut(&mut T) -> io::Result<R>) -> io::Result<R> {
        loop {
            self.check()?;
            match operation(&mut self.inner) {
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                result => return result,
            }
        }
    }
}

impl<T: Read> Read for Pipe<T> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.retry(|inner| inner.read(bytes))
    }
}

impl<T: Write> Write for Pipe<T> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.retry(|inner| inner.write(bytes))
    }
    fn flush(&mut self) -> io::Result<()> {
        self.retry(Write::flush)
    }
}
