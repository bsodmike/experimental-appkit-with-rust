//! A running terminal: a shell on a pty, a reader thread, and the engine.
//!
//! This is PRD §9's path B end to end. The reader thread blocks on the pty,
//! parses what comes back *on that thread* (the UI thread must never parse:
//! parsing is unbounded work driven by a hostile-by-accident data source), and
//! writes any reply the program is owed straight back down the pty. The UI
//! thread only ever takes a frame.
//!
//! It is also PRD §7's ordered shutdown, in one place: silence the wake-up,
//! interrupt the reader, join it, and only then hang up the child. Nothing is
//! left running behind a handle that is about to go away, and `Drop` does the
//! same thing for the caller who forgets.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

use terminal_core::prelude::{Frame, Key, Modifiers, Session, TerminalSize};

use crate::interrupt::Interrupt;
use crate::pty::{ChildOutcome, Pty, SpawnOptions};

/// How long a child gets to honour `SIGHUP` before it is killed.
const SHUTDOWN_GRACE: Duration = Duration::from_millis(500);

/// How much output one read may return. Large enough that a burst is a handful
/// of reads rather than hundreds, small enough that the lock is never held long.
const READ_BUFFER_BYTES: usize = 8192;

/// A shell running on a pty, with the engine attached to it.
pub struct Terminal {
    session: Session,
    /// The writer side. The reader thread holds its own handle to the same
    /// descriptor, so this lock is never held while anything blocks.
    pty: Mutex<Pty>,
    interrupt: Interrupt,
    /// Set when the reader thread stops: the child hung up, or shutdown asked
    /// it to. The UI reads this to know the window has nothing left to show.
    hung_up: Arc<AtomicBool>,
    reader: Mutex<Option<JoinHandle<()>>>,
}

impl Terminal {
    /// Spawn `program` on a new pty of `size` and start reading from it.
    ///
    /// `wake_up` is called when the screen goes from clean to dirty — once per
    /// burst, not once per read (PRD §7). It runs on the reader thread with no
    /// lock held, and the frontend's version does nothing but ask the main
    /// thread to redraw.
    pub fn spawn<S: AsRef<std::ffi::OsStr>>(
        program: S,
        args: &[S],
        size: TerminalSize,
        wake_up: impl Fn() + Send + Sync + 'static,
    ) -> io::Result<Self> {
        let options = SpawnOptions::new(program.as_ref()).args(args.iter().map(AsRef::as_ref));
        Self::spawn_with(&options, size, wake_up)
    }

    /// Spawn with full control over the child's environment and directory —
    /// what a frontend uses, since an app bundle's own environment is not one a
    /// shell can work in.
    pub fn spawn_with(
        options: &SpawnOptions,
        size: TerminalSize,
        wake_up: impl Fn() + Send + Sync + 'static,
    ) -> io::Result<Self> {
        let pty = Pty::spawn_with(options, size)?;
        let mut reader_handle = pty.try_clone_handle()?;
        let session = Session::with_wake_up(size, wake_up);
        let interrupt = Interrupt::new()?;
        let hung_up = Arc::new(AtomicBool::new(false));

        let thread = {
            let session = session.clone();
            let interrupt = interrupt.clone();
            let hung_up = Arc::clone(&hung_up);
            std::thread::Builder::new()
                .name("pty-reader".to_string())
                .spawn(move || {
                    let mut buf = [0u8; READ_BUFFER_BYTES];
                    loop {
                        let n = match reader_handle.read_interruptible(&mut buf, &interrupt) {
                            Ok(0) => break,
                            Ok(n) => n,
                            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                            Err(_) => break,
                        };
                        // Parse on this thread, under the engine's lock, and
                        // answer the program on the way out (PRD §9).
                        let replies = session.feed(&buf[..n]);
                        if !replies.is_empty() && reader_handle.write_all(&replies).is_err() {
                            break;
                        }
                    }
                    hung_up.store(true, Ordering::Release);
                    // Mark the screen dirty so the UI redraws once more and
                    // notices the shell has gone.
                    session.update(|_| {});
                })?
        };

        Ok(Self {
            session,
            pty: Mutex::new(pty),
            interrupt,
            hung_up,
            reader: Mutex::new(Some(thread)),
        })
    }

    /// The engine handle, for anything the frontend needs beyond a frame.
    pub fn session(&self) -> &Session {
        &self.session
    }

    /// Send input to the shell: the bytes a keystroke encoded to (PRD §8).
    pub fn send(&self, bytes: &[u8]) -> io::Result<()> {
        self.pty().write_all(bytes)
    }

    /// Send a keystroke: the engine encodes it against the current modes
    /// (PRD §8), and the bytes go down the pty.
    pub fn send_key(&self, key: Key, mods: Modifiers) -> io::Result<()> {
        let bytes = self.session.encode_key(key, mods);
        if bytes.is_empty() {
            return Ok(());
        }
        self.send(&bytes)
    }

    /// Send committed text from the input system — the other input channel of
    /// PRD §8, which needs no encoding because it is already UTF-8.
    pub fn send_text(&self, text: &str) -> io::Result<()> {
        self.send(text.as_bytes())
    }

    /// Send pasted text, bracketed if the program asked for that.
    pub fn paste(&self, text: &str) -> io::Result<()> {
        let bytes = self.session.encode_paste(text);
        self.send(&bytes)
    }

    /// Resize both halves: the engine reflows, and the kernel delivers
    /// `SIGWINCH` so the program redraws (PRD §16.4).
    ///
    /// The engine goes first. A program that reacts to `SIGWINCH` by redrawing
    /// immediately would otherwise have its output land in a screen still laid
    /// out for the old width.
    pub fn resize(&self, size: TerminalSize) -> io::Result<()> {
        self.session.resize(size);
        self.pty().resize(size)
    }

    /// Copy the visible screen into `frame` (PRD §10-A).
    pub fn render_into(&self, frame: &mut Frame) {
        self.session.render_into(frame);
    }

    /// The shell's process id.
    pub fn child_id(&self) -> u32 {
        self.pty().child_id()
    }

    /// Whether the shell has gone and the reader thread has stopped.
    pub fn has_hung_up(&self) -> bool {
        self.hung_up.load(Ordering::Acquire)
    }

    /// How the shell ended, if it has ended and can still be asked.
    ///
    /// `None` while it is still running — and also when the reader thread
    /// stopped for its own reasons rather than the shell's, which is the case a
    /// frontend must not read as a clean exit.
    pub fn child_outcome(&self) -> Option<ChildOutcome> {
        self.pty().try_status().ok().flatten()
    }

    /// Stop everything, in the order PRD §7 requires: silence the wake-up so no
    /// callback can be in flight, interrupt the reader so it leaves its
    /// `poll`, join it, and only then hang up the child and reap it.
    ///
    /// Idempotent: calling it twice, or after the shell has already exited, is
    /// fine.
    pub fn shutdown(&self) -> io::Result<std::process::ExitStatus> {
        self.session.shutdown();
        self.interrupt.fire();
        if let Some(thread) = self.reader.lock().unwrap_or_else(|e| e.into_inner()).take() {
            // A reader thread that panicked has already stopped, which is all
            // this step needs to establish.
            let _ = thread.join();
        }
        self.pty().shutdown(SHUTDOWN_GRACE)
    }

    fn pty(&self) -> MutexGuard<'_, Pty> {
        self.pty.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        // The reader thread writes into memory this struct owns, so it must not
        // outlive it — even when the caller forgot to say so.
        let _ = self.shutdown();
    }
}

impl std::fmt::Debug for Terminal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Terminal")
            .field("session", &self.session)
            .field("hung_up", &self.has_hung_up())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::time::Instant;

    const SIZE: TerminalSize = TerminalSize::new(10, 40);

    fn sh(script: &str) -> Terminal {
        Terminal::spawn("/bin/sh", &["-c", script], SIZE, || {}).expect("spawn")
    }

    /// Wait for `cond`, failing the test rather than hanging forever.
    fn wait_for(what: &str, mut cond: impl FnMut() -> bool) {
        // Generous, because these tests share a machine with every other
        // test in the workspace: a slow answer is not a wrong one.
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            if cond() {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("timed out waiting for {what}");
    }

    fn screen_text(terminal: &Terminal) -> String {
        let mut frame = Frame::new();
        terminal.render_into(&mut frame);
        (0..frame.size().rows)
            .map(|r| frame.row_text(r))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn output_from_the_shell_lands_on_the_screen() {
        let terminal = sh("echo hello");
        wait_for("the shell to hang up", || terminal.has_hung_up());
        assert!(screen_text(&terminal).starts_with("hello"));
    }

    #[test]
    fn input_reaches_the_shell_and_its_answer_comes_back() {
        let terminal = sh("stty -echo; read line; echo got:$line");
        terminal.send(b"typed\n").expect("send");
        wait_for("the shell to hang up", || terminal.has_hung_up());
        assert!(
            screen_text(&terminal).contains("got:typed"),
            "screen was: {:?}",
            screen_text(&terminal)
        );
    }

    #[test]
    fn a_burst_of_output_wakes_the_ui_far_less_often_than_it_reads() {
        let wakes = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&wakes);
        let terminal = Terminal::spawn(
            "/bin/sh",
            &[
                "-c",
                "i=0; while [ $i -lt 300 ]; do echo line $i; i=$((i+1)); done",
            ],
            SIZE,
            move || {
                counter.fetch_add(1, Ordering::SeqCst);
            },
        )
        .expect("spawn");

        wait_for("the shell to hang up", || terminal.has_hung_up());
        let woken = wakes.load(Ordering::SeqCst);
        // Nothing takes a frame here, so after the first wake-up the flag stays
        // set: 300 lines of output cost one redraw, plus the one that reports
        // the hangup.
        assert!(woken <= 2, "woke the UI {woken} times for one burst");
        assert!(screen_text(&terminal).contains("line 299"));
    }

    #[test]
    fn the_engine_answers_the_shell_upstream() {
        // The shell asks where the cursor is and reads the six bytes of the
        // answer back off its own stdin; `cat -v` then prints them visibly.
        // -icanon matters: the answer has no newline, so a line-buffered read
        // would wait for one that never comes.
        let terminal = sh("stty -echo -icanon; printf '\\033[6n'; head -c 6 | cat -v");
        wait_for("the shell to hang up", || terminal.has_hung_up());
        let text = screen_text(&terminal);
        assert!(text.contains("[1;1R"), "screen was: {text:?}");
    }

    #[test]
    fn a_keystroke_reaches_the_shell_as_the_bytes_the_terminal_defines() {
        // -isig so Ctrl+C is delivered as a byte rather than as a signal; the
        // "ready" handshake keeps the keystrokes from arriving before stty has
        // run, when the line discipline would still swallow them.
        let terminal = sh("stty -echo -icanon -isig; echo ready; head -c 4 | cat -v");
        wait_for("the shell to be listening", || {
            screen_text(&terminal).contains("ready")
        });
        terminal
            .send_key(Key::Up, Modifiers::NONE)
            .expect("send arrow");
        terminal
            .send_key(Key::Char('c'), Modifiers::CTRL)
            .expect("send ctrl-c");
        wait_for("the shell to hang up", || terminal.has_hung_up());
        let text = screen_text(&terminal);
        assert!(text.contains("^[[A^C"), "screen was: {text:?}");
    }

    #[test]
    fn a_resize_reaches_both_the_engine_and_the_kernel() {
        let terminal = sh("read x; stty size");
        terminal.resize(TerminalSize::new(20, 60)).expect("resize");
        terminal.send(b"\n").expect("send");
        wait_for("the shell to hang up", || terminal.has_hung_up());
        assert!(
            screen_text(&terminal).contains("20 60"),
            "screen was: {:?}",
            screen_text(&terminal)
        );
        let mut frame = Frame::new();
        terminal.render_into(&mut frame);
        assert_eq!(frame.size(), TerminalSize::new(20, 60));
    }

    #[test]
    fn shutdown_stops_a_shell_that_would_have_run_all_day() {
        let terminal = sh("sleep 300");
        let started = Instant::now();
        terminal.shutdown().expect("shutdown");
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(!terminal.session().is_running());
    }

    #[test]
    fn a_clean_exit_is_distinguishable_from_a_crash() {
        // The frontend closes the window on a clean exit and keeps it open on
        // anything else, so this distinction is the whole rule.
        let clean = sh("exit 0");
        wait_for("a clean exit", || clean.has_hung_up());
        assert_eq!(clean.child_outcome(), Some(ChildOutcome::Code(0)));
        assert!(clean.child_outcome().unwrap().is_clean());

        let failed = sh("exit 3");
        wait_for("a failed exit", || failed.has_hung_up());
        assert_eq!(failed.child_outcome(), Some(ChildOutcome::Code(3)));
        assert!(!failed.child_outcome().unwrap().is_clean());
    }

    #[test]
    fn a_signalled_child_reports_its_signal() {
        let terminal = sh("kill -TERM $$");
        wait_for("the signal", || terminal.has_hung_up());
        assert_eq!(terminal.child_outcome(), Some(ChildOutcome::Signal(15)));
        assert!(!terminal.child_outcome().unwrap().is_clean());
    }

    #[test]
    fn a_running_shell_has_no_outcome_yet() {
        let terminal = sh("sleep 30");
        assert_eq!(terminal.child_outcome(), None);
        assert!(!terminal.has_hung_up());
    }

    #[test]
    fn the_outcome_survives_being_asked_twice() {
        // try_wait consumes the status; asking again must not turn a clean exit
        // into "still running", which is what the frontend would draw.
        let terminal = sh("exit 7");
        wait_for("the exit", || terminal.has_hung_up());
        assert_eq!(terminal.child_outcome(), Some(ChildOutcome::Code(7)));
        assert_eq!(terminal.child_outcome(), Some(ChildOutcome::Code(7)));
    }

    #[test]
    fn shutdown_is_idempotent() {
        let terminal = sh("sleep 300");
        terminal.shutdown().expect("first");
        terminal.shutdown().expect("second");
    }

    #[test]
    fn dropping_a_terminal_stops_the_shell_it_started() {
        // Nothing may outlive the handle it belongs to (PRD §7), and the drop
        // has to join the reader thread before it can reap the child at all.
        let pid;
        {
            let terminal = sh("sleep 300");
            pid = terminal.child_id();
            assert!(process_exists(pid));
        }
        assert!(!process_exists(pid), "the shell outlived its terminal");
    }

    fn process_exists(pid: u32) -> bool {
        std::path::Path::new(&format!("/proc/{pid}")).exists()
    }
}
