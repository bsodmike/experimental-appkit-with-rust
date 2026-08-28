//! The pty itself: a descriptor, a child process, and a window size.

use std::ffi::{OsStr, OsString};
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::Child;

use terminal_core::prelude::TerminalSize;

use crate::interrupt::Interrupt;

/// What `TERM` the engine claims to be.
///
/// This is the engine's own capability statement, not the frontend's decoration
/// (PRD §5): programs look the name up in terminfo to decide what sequences they
/// may send, so it has to agree with what the VT parser actually implements.
/// `xterm-256color` is the pragmatic choice every terminal makes — it is present
/// on every machine, where a bespoke terminfo entry would have to be installed
/// first — and it promises a little more than we implement today (mouse
/// reporting, DCS). A caller who knows better can override it.
pub const DEFAULT_TERM: &str = "xterm-256color";

/// Advertises 24-bit colour, which the engine does support: SGR 38/48;2;r;g;b
/// round-trips through the parser and out through the render runs.
pub const DEFAULT_COLORTERM: &str = "truecolor";

/// How a child process ended.
///
/// The distinction matters to a frontend: a shell that exited cleanly is a
/// session the user finished, and one that was killed or failed is a screen the
/// user probably wants to keep reading.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ChildOutcome {
    /// Exited with this status. Zero is the ordinary end of a shell session.
    Code(i32),
    /// Killed by this signal.
    Signal(i32),
}

impl ChildOutcome {
    fn from_status(status: std::process::ExitStatus) -> Self {
        use std::os::unix::process::ExitStatusExt;
        match (status.code(), status.signal()) {
            (Some(code), _) => Self::Code(code),
            (None, Some(signal)) => Self::Signal(signal),
            // Neither, which POSIX does not produce; reported as a failure
            // rather than silently as success.
            (None, None) => Self::Code(-1),
        }
    }

    /// Whether this is the ordinary end of a session.
    pub fn is_clean(self) -> bool {
        matches!(self, Self::Code(0))
    }
}

/// How to start the child process.
///
/// An app bundle launched from Finder has almost no environment — no `TERM`, a
/// stub `PATH` — so a shell started from one needs to be told where it is and
/// what it is talking to, or `vim` and `less` refuse to run.
#[derive(Clone, Debug, Default)]
pub struct SpawnOptions {
    program: OsString,
    args: Vec<OsString>,
    cwd: Option<PathBuf>,
    env: Vec<(OsString, OsString)>,
}

impl SpawnOptions {
    /// Run `program`, inheriting this process's environment and directory.
    pub fn new(program: impl Into<OsString>) -> Self {
        Self {
            program: program.into(),
            ..Self::default()
        }
    }

    /// Append one argument. A login shell wants `-l`, which is what rebuilds
    /// `PATH` from the user's profile when the app was launched from Finder.
    pub fn arg(mut self, arg: impl Into<OsString>) -> Self {
        self.args.push(arg.into());
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    /// The directory the child starts in. Left unset, it inherits this
    /// process's — which for an app bundle is `/`, so a frontend will want to
    /// say otherwise.
    pub fn cwd(mut self, dir: impl Into<PathBuf>) -> Self {
        self.cwd = Some(dir.into());
        self
    }

    /// Set one environment variable, overriding both the inherited value and
    /// the defaults above.
    pub fn env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    pub fn program(&self) -> &OsStr {
        &self.program
    }
}

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
    /// Spawn `program` with `args` on a new pty of `size`, inheriting this
    /// process's environment and directory.
    ///
    /// The child becomes the leader of its own session with the pty as its
    /// controlling terminal, which is what makes job control, `Ctrl+C` and
    /// `isatty()` behave the way a shell expects.
    pub fn spawn<S: AsRef<OsStr>>(program: S, args: &[S], size: TerminalSize) -> io::Result<Self> {
        let options = SpawnOptions::new(program.as_ref()).args(args.iter().map(AsRef::as_ref));
        Self::spawn_with(&options, size)
    }

    /// Spawn with full control over the child's environment and directory.
    ///
    /// [`DEFAULT_TERM`] and [`DEFAULT_COLORTERM`] are always set, before the
    /// caller's own entries, so a frontend gets a working terminal without
    /// having to know the answer — and can still override it if it does.
    pub fn spawn_with(options: &SpawnOptions, size: TerminalSize) -> io::Result<Self> {
        let (pty, pts) = pty_process::blocking::open().map_err(io::Error::other)?;
        // Set the size before the child starts, so it never sees the 0x0 that a
        // fresh pty reports and lays itself out for a screen that never was.
        pty.resize(to_pty_size(size)).map_err(io::Error::other)?;

        let mut command = pty_process::blocking::Command::new(&options.program)
            .args(options.args.iter())
            .env("TERM", DEFAULT_TERM)
            .env("COLORTERM", DEFAULT_COLORTERM);
        for (key, value) in &options.env {
            command = command.env(key, value);
        }
        if let Some(cwd) = &options.cwd {
            command = command.current_dir(cwd);
        }

        let child = command.spawn(pts).map_err(io::Error::other)?;
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
        Ok(self.try_status()?.is_some())
    }

    /// How the child ended, if it has, without blocking.
    ///
    /// This reaps it, so the answer is remembered rather than asked again — a
    /// process that has been waited for once has no status to give a second
    /// time, and callers should not have to know that.
    pub fn try_status(&mut self) -> io::Result<Option<ChildOutcome>> {
        Ok(self.child.try_wait()?.map(ChildOutcome::from_status))
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
    fn the_child_is_told_what_terminal_it_is_talking_to() {
        // Without this an app launched from Finder gives its shell no TERM at
        // all, and vim, less and top refuse to start.
        let mut pty = sh(
            "printf '%s|%s' \"$TERM\" \"$COLORTERM\"",
            TerminalSize::new(24, 80),
        );
        assert_eq!(read_to_end(&mut pty), "xterm-256color|truecolor");
    }

    #[test]
    fn a_caller_can_override_the_defaults() {
        let options = SpawnOptions::new("/bin/sh")
            .arg("-c")
            .arg("printf '%s' \"$TERM\"")
            .env("TERM", "dumb");
        let mut pty = Pty::spawn_with(&options, TerminalSize::new(24, 80)).expect("spawn");
        assert_eq!(read_to_end(&mut pty), "dumb");
    }

    #[test]
    fn extra_environment_reaches_the_child() {
        let options = SpawnOptions::new("/bin/sh")
            .args(["-c", "printf '%s' \"$GRILL_TEST\""])
            .env("GRILL_TEST", "hello");
        let mut pty = Pty::spawn_with(&options, TerminalSize::new(24, 80)).expect("spawn");
        assert_eq!(read_to_end(&mut pty), "hello");
    }

    #[test]
    fn the_child_starts_in_the_directory_it_was_given() {
        let options = SpawnOptions::new("/bin/sh").args(["-c", "pwd"]).cwd("/usr");
        let mut pty = Pty::spawn_with(&options, TerminalSize::new(24, 80)).expect("spawn");
        assert_eq!(read_to_end(&mut pty).trim(), "/usr");
    }

    #[test]
    fn without_a_directory_the_child_inherits_ours() {
        let options = SpawnOptions::new("/bin/sh").args(["-c", "pwd"]);
        let mut pty = Pty::spawn_with(&options, TerminalSize::new(24, 80)).expect("spawn");
        let expected = std::env::current_dir().expect("cwd");
        assert_eq!(read_to_end(&mut pty).trim(), expected.to_str().unwrap());
    }

    #[test]
    fn the_rest_of_the_environment_is_still_inherited() {
        // Setting TERM must not mean starting from an empty environment: PATH,
        // HOME and the rest still have to be there.
        let mut pty = sh("printf '%s' \"$HOME\"", TerminalSize::new(24, 80));
        let expected = std::env::var("HOME").expect("HOME");
        assert_eq!(read_to_end(&mut pty), expected);
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
