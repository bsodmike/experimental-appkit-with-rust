//! The pty itself: a descriptor, a child process, and a window size.

use std::io::{self, Read, Write};
use std::process::Child;

use terminal_core::prelude::TerminalSize;

use crate::interrupt::Interrupt;

/// A pseudo-terminal with a child process running on the far end.
///
/// Dropping a `Pty` closes the descriptor but does *not* wait for the child;
/// see [`Pty::shutdown`] for the ordered version PRD §7 asks for.
pub struct Pty {
    handle: PtyHandle,
    child: Child,
}

impl std::fmt::Debug for Pty {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The descriptor is not printable; the child's id is what a log wants.
        f.debug_struct("Pty")
            .field("child_id", &self.child.id())
            .finish()
    }
}

/// A handle to a pty descriptor, with no claim on the child process.
///
/// [`Pty::try_clone_handle`] makes a second one for the reader thread. Both
/// refer to the same open file description, so a write on one and a blocking
/// read on the other never wait for each other — which they would if the reader
/// thread held a lock over the descriptor while parked in `poll`.
pub struct PtyHandle(pty_process::blocking::Pty);

impl std::fmt::Debug for PtyHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use std::os::fd::AsRawFd;
        f.debug_struct("PtyHandle")
            .field("fd", &self.0.as_raw_fd())
            .finish()
    }
}

impl PtyHandle {
    /// Another handle to the same descriptor.
    pub fn try_clone(&self) -> io::Result<Self> {
        use std::os::fd::AsFd;
        let fd = self.0.as_fd().try_clone_to_owned()?;
        // Safety: the descriptor is a live pty master, duplicated just above.
        Ok(Self(unsafe { pty_process::blocking::Pty::from_fd(fd) }))
    }

    /// Read output from the child. `Ok(0)` means the far end has gone.
    ///
    /// Once the child exits, the kernel reports `EIO` on the master rather than
    /// a clean end-of-file. That is this API's end-of-file, and translating it
    /// here keeps the distinction out of every caller.
    pub fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.0.read(buf) {
            Err(e) if is_hangup(&e) => Ok(0),
            other => other,
        }
    }

    /// Read output, giving up if `interrupt` fires first. `Ok(0)` means either
    /// the child has gone or the interrupt fired: both mean "stop reading".
    ///
    /// This is what makes the reader thread joinable. A thread blocked in a
    /// plain `read()` cannot be asked to stop, and closing the descriptor under
    /// it races with descriptor reuse.
    pub fn read_interruptible(
        &mut self,
        buf: &mut [u8],
        interrupt: &Interrupt,
    ) -> io::Result<usize> {
        use rustix::event::{PollFd, PollFlags, poll};
        loop {
            let interrupt_fd = interrupt.as_fd();
            let mut fds = [
                PollFd::new(&self.0, PollFlags::IN),
                PollFd::new(&interrupt_fd, PollFlags::IN),
            ];
            match poll(&mut fds, None) {
                Ok(_) => {}
                Err(rustix::io::Errno::INTR) => continue, // a signal, not an answer
                Err(e) => return Err(e.into()),
            }
            if !fds[1].revents().is_empty() {
                return Ok(0);
            }
            let revents = fds[0].revents();
            if revents.contains(PollFlags::IN) {
                return self.read(buf);
            }
            if !revents.is_empty() {
                // The child has gone (HUP/ERR). Drain what it left before
                // saying so: output written just before exit still matters.
                return self.read(buf);
            }
        }
    }

    /// Send input to the child.
    pub fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.0.write_all(bytes)
    }

    /// Tell the kernel the window size, which is what makes it deliver
    /// `SIGWINCH` to the child so it redraws (PRD §16.4).
    pub fn resize(&self, size: TerminalSize) -> io::Result<()> {
        self.0.resize(to_pty_size(size)).map_err(io::Error::other)
    }
}

impl Pty {
    /// Spawn `program` with `args` on a new pty of `size`.
    ///
    /// The child becomes the leader of its own session with the pty as its
    /// controlling terminal, which is what makes job control, `Ctrl+C` and
    /// `isatty()` behave the way a shell expects.
    pub fn spawn<S: AsRef<std::ffi::OsStr>>(
        program: S,
        args: &[S],
        size: TerminalSize,
    ) -> io::Result<Self> {
        let (pty, pts) = pty_process::blocking::open().map_err(io::Error::other)?;
        // Set the size before the child starts, so it never sees the 0x0 that a
        // fresh pty reports and lays itself out for a screen that never was.
        pty.resize(to_pty_size(size)).map_err(io::Error::other)?;
        let child = pty_process::blocking::Command::new(program)
            .args(args.iter())
            .spawn(pts)
            .map_err(io::Error::other)?;
        Ok(Self {
            handle: PtyHandle(pty),
            child,
        })
    }

    /// A second handle to the same descriptor, for the reader thread.
    pub fn try_clone_handle(&self) -> io::Result<PtyHandle> {
        self.handle.try_clone()
    }

    /// Read output from the child. `Ok(0)` means the far end has gone.
    pub fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.handle.read(buf)
    }

    /// Read output, giving up if `interrupt` fires first.
    pub fn read_interruptible(
        &mut self,
        buf: &mut [u8],
        interrupt: &Interrupt,
    ) -> io::Result<usize> {
        self.handle.read_interruptible(buf, interrupt)
    }

    /// Send input to the child.
    pub fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.handle.write_all(bytes)
    }

    /// Tell the kernel the window size (PRD §16.4).
    pub fn resize(&self, size: TerminalSize) -> io::Result<()> {
        self.handle.resize(size)
    }

    /// The child's process id.
    pub fn child_id(&self) -> u32 {
        self.child.id()
    }

    /// Whether the child has already exited, without blocking.
    pub fn child_has_exited(&mut self) -> io::Result<bool> {
        Ok(self.child.try_wait()?.is_some())
    }

    /// Ask the child to go, then wait for it.
    ///
    /// `SIGHUP` first, because that is what a terminal closing means and a shell
    /// knows how to handle it — it hangs up its own children in turn. A child
    /// that ignores it is killed outright after `grace`; leaving it running
    /// would leave the pty alive and the reader thread with no reason to stop,
    /// which is exactly the shutdown PRD §7 forbids.
    pub fn shutdown(&mut self, grace: std::time::Duration) -> io::Result<std::process::ExitStatus> {
        if let Some(status) = self.child.try_wait()? {
            return Ok(status);
        }
        let pid = rustix::process::Pid::from_child(&self.child);
        let _ = rustix::process::kill_process(pid, rustix::process::Signal::HUP);

        let deadline = std::time::Instant::now() + grace;
        loop {
            if let Some(status) = self.child.try_wait()? {
                return Ok(status);
            }
            if std::time::Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        self.child.kill()?;
        self.child.wait()
    }
}

fn to_pty_size(size: TerminalSize) -> pty_process::Size {
    pty_process::Size::new(size.rows, size.cols)
}

/// Whether an error is the far end going away rather than a real failure.
fn is_hangup(e: &io::Error) -> bool {
    e.raw_os_error() == Some(rustix::io::Errno::IO.raw_os_error())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const GRACE: Duration = Duration::from_millis(500);

    /// Read until the child hangs up, as the reader thread does.
    fn read_to_end(pty: &mut Pty) -> String {
        let mut out = Vec::new();
        let mut buf = [0u8; 1024];
        loop {
            match pty.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => out.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => panic!("read failed: {e}"),
            }
        }
        String::from_utf8_lossy(&out).into_owned()
    }

    fn sh(script: &str, size: TerminalSize) -> Pty {
        Pty::spawn("/bin/sh", &["-c", script], size).expect("spawn")
    }

    #[test]
    fn output_from_the_child_comes_back_through_the_pty() {
        let mut pty = sh("echo hello", TerminalSize::new(24, 80));
        // A pty is line-disciplined, so the newline arrives as CR LF -- which is
        // exactly why the engine's parser has to handle both.
        assert_eq!(read_to_end(&mut pty), "hello\r\n");
    }

    #[test]
    fn input_written_to_the_pty_reaches_the_child() {
        let mut pty = sh("read line; echo got:$line", TerminalSize::new(24, 80));
        pty.write_all(b"typed\n").expect("write");
        let out = read_to_end(&mut pty);
        assert!(out.contains("got:typed"), "unexpected output: {out:?}");
    }

    #[test]
    fn the_child_sees_the_window_size_it_was_given() {
        // `stty size` asks the kernel, so this proves the size reached the
        // descriptor and not merely our own struct.
        let mut pty = sh("stty size", TerminalSize::new(30, 100));
        assert_eq!(read_to_end(&mut pty).trim(), "30 100");
    }

    #[test]
    fn a_resize_is_visible_to_a_running_child() {
        let mut pty = sh("read x; stty size", TerminalSize::new(24, 80));
        pty.resize(TerminalSize::new(40, 120)).expect("resize");
        pty.write_all(b"\n").expect("write");
        let out = read_to_end(&mut pty);
        assert!(out.contains("40 120"), "unexpected output: {out:?}");
    }

    #[test]
    fn the_child_is_a_session_leader_with_a_controlling_terminal() {
        // If it were not, `tty` would print "not a tty" and every full-screen
        // program would behave as though it were writing to a file.
        let mut pty = sh("tty", TerminalSize::new(24, 80));
        let out = read_to_end(&mut pty);
        assert!(out.contains("/dev/pts/"), "unexpected output: {out:?}");
    }

    #[test]
    fn reading_ends_when_the_child_exits() {
        let mut pty = sh("exit 0", TerminalSize::new(24, 80));
        let mut buf = [0u8; 64];
        // EIO on the master is this API's end-of-file.
        while pty.read(&mut buf).expect("read") != 0 {}
        assert!(pty.child_has_exited().expect("wait"));
    }

    #[test]
    fn an_interrupt_unblocks_a_reader_waiting_on_a_silent_child() {
        let mut pty = sh("sleep 30", TerminalSize::new(24, 80));
        let interrupt = Interrupt::new().unwrap();
        let firing = interrupt.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            firing.fire();
        });

        let mut buf = [0u8; 64];
        let started = std::time::Instant::now();
        assert_eq!(pty.read_interruptible(&mut buf, &interrupt).unwrap(), 0);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "it did not wait for the sleep"
        );
        pty.shutdown(GRACE).expect("shutdown");
    }

    #[test]
    fn an_interruptible_read_still_returns_output() {
        let mut pty = sh("echo hi", TerminalSize::new(24, 80));
        let interrupt = Interrupt::new().unwrap();
        let mut buf = [0u8; 64];
        let n = pty.read_interruptible(&mut buf, &interrupt).expect("read");
        assert_eq!(&buf[..n], b"hi\r\n");
    }

    #[test]
    fn shutdown_hangs_up_a_child_that_would_otherwise_run_on() {
        let mut pty = sh("sleep 30", TerminalSize::new(24, 80));
        let started = std::time::Instant::now();
        pty.shutdown(GRACE).expect("shutdown");
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(pty.child_has_exited().expect("wait"));
    }

    #[test]
    fn shutdown_kills_a_child_that_ignores_the_hangup() {
        // The grace period is the whole point: a child that traps SIGHUP must
        // not be able to keep the pty, and the reader thread, alive.
        let mut pty = sh("trap '' HUP; sleep 30", TerminalSize::new(24, 80));
        std::thread::sleep(Duration::from_millis(100)); // let the trap be set
        pty.shutdown(Duration::from_millis(100)).expect("shutdown");
        assert!(pty.child_has_exited().expect("wait"));
    }

    #[test]
    fn a_cloned_handle_reads_the_same_pty() {
        // The reader thread gets one of these while the writer keeps the other,
        // so a blocking read never holds anything the writer needs.
        let pty = sh("echo shared", TerminalSize::new(24, 80));
        let mut handle = pty.try_clone_handle().expect("clone");
        let mut buf = [0u8; 64];
        let n = handle.read(&mut buf).expect("read");
        assert_eq!(&buf[..n], b"shared\r\n");
    }

    #[test]
    fn a_child_that_has_already_exited_keeps_its_own_status() {
        // Shutdown reaps rather than signals when there is nothing left to
        // signal, so the child's real exit code survives -- and asking twice
        // gives the same answer.
        let mut pty = sh("exit 3", TerminalSize::new(24, 80));
        read_to_end(&mut pty); // read to the hangup: the child is gone by now
        let first = pty.shutdown(GRACE).expect("shutdown");
        let second = pty.shutdown(GRACE).expect("shutdown again");
        assert_eq!(first.code(), Some(3));
        assert_eq!(second.code(), first.code());
    }
}
