//! A way to wake a thread that is blocked reading the PTY.
//!
//! PRD §7 requires an ordered shutdown: the reader thread is *signalled* and
//! *joined* before the handle it writes into is destroyed. A thread blocked in
//! `read()` on the pty cannot be signalled by a flag it never looks at, and
//! closing the descriptor out from under it races with descriptor reuse. The
//! standard answer is a second descriptor to wait on: a pipe nobody writes to
//! except at shutdown.

use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::sync::Arc;

/// A one-shot wake-up for a blocked reader. Cloning gives another handle to the
/// same pipe, so the thread doing the shutdown and the thread being woken can
/// each hold one.
#[derive(Clone, Debug)]
pub struct Interrupt {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    read: OwnedFd,
    write: OwnedFd,
}

impl Interrupt {
    /// Create an interrupt that has not fired.
    pub fn new() -> io::Result<Self> {
        let (read, write) = rustix::pipe::pipe()?;
        Ok(Self {
            inner: Arc::new(Inner { read, write }),
        })
    }

    /// Wake whoever is waiting, now and for good: the pipe stays readable, so a
    /// reader that has not reached its wait yet still sees it. Calling this more
    /// than once is harmless.
    pub fn fire(&self) {
        // A full pipe already means "fired", and a closed one means nobody is
        // listening; neither is worth reporting.
        let _ = rustix::io::write(&self.inner.write, b"!");
    }

    /// Whether the interrupt has fired.
    pub fn has_fired(&self) -> bool {
        // Poll with no timeout: readable means a byte is waiting.
        let mut fds = [rustix::event::PollFd::new(
            &self.inner.read,
            rustix::event::PollFlags::IN,
        )];
        matches!(
            rustix::event::poll(&mut fds, Some(&rustix::event::Timespec::default())),
            Ok(n) if n > 0
        )
    }

    /// The descriptor to wait on alongside the pty.
    pub(crate) fn as_fd(&self) -> BorrowedFd<'_> {
        self.inner.read.as_fd()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_interrupt_starts_unfired_and_stays_fired() {
        let interrupt = Interrupt::new().unwrap();
        assert!(!interrupt.has_fired());
        interrupt.fire();
        assert!(interrupt.has_fired());
        assert!(interrupt.has_fired(), "reading it does not consume it");
    }

    #[test]
    fn firing_twice_is_harmless() {
        let interrupt = Interrupt::new().unwrap();
        interrupt.fire();
        interrupt.fire();
        assert!(interrupt.has_fired());
    }

    #[test]
    fn clones_share_one_pipe() {
        let interrupt = Interrupt::new().unwrap();
        let other = interrupt.clone();
        other.fire();
        assert!(interrupt.has_fired(), "either handle fires it for both");
    }
}
