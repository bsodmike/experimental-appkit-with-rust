//! The synchronised terminal: one lock, one wake-up flag, one shutdown.
//!
//! PRD §7: the engine is internally synchronised — `Arc<Mutex<..>>` behind an
//! opaque handle — so that from the frontend's point of view there is no lock at
//! all. Exposing lock/unlock across the FFI would make deadlock the frontend's
//! problem to solve.
//!
//! Two threads meet here. The PTY reader calls [`Session::feed`], which takes
//! the lock, parses, mutates and releases; the UI thread calls
//! [`Session::render_into`], which takes the lock, copies one frame and
//! releases. The lock is held for microseconds and never across the wake-up
//! callback — a callback that re-entered the terminal API while the lock was
//! held would deadlock on a non-reentrant mutex.
//!
//! ## The coalesced wake-up
//!
//! `cat` of a large file produces thousands of small reads. If every one asked
//! the UI to redraw, the main thread would spend its time draining a backlog of
//! redundant redraws and the app would appear to hang under exactly the workload
//! it should handle best. Only the `false -> true` transition of the dirty flag
//! calls the wake-up; every chunk after that sets a flag that is already set and
//! costs nothing. The UI always draws the latest state, and at most one redraw
//! is ever pending.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::geometry::TerminalSize;
use crate::keys::{Key, Modifiers};
use crate::parsers::vt::VtParser;
use crate::render::Frame;
use crate::screen::Screen;

/// What the lock protects: the screen and the parser state that must stay in
/// step with it. The parser is stateful across reads (PRD §9), so it belongs
/// inside the same critical section as the screen it drives.
struct Emulator {
    screen: Screen,
    parser: VtParser,
}

struct Shared {
    emulator: Mutex<Emulator>,
    /// Set when the screen has changed since the last frame was taken. Only its
    /// `false -> true` transition posts a redraw.
    dirty: AtomicBool,
    /// Cleared by [`Session::shutdown`]. Once clear, no wake-up is ever fired
    /// again: after shutdown the frontend's callback may point at memory that
    /// is on its way out.
    running: AtomicBool,
    wake_up: Box<dyn Fn() + Send + Sync>,
}

/// A thread-safe handle to one terminal. Cloning gives another handle to the
/// same terminal, which is how the reader thread and the UI thread share it.
#[derive(Clone)]
pub struct Session {
    shared: Arc<Shared>,
}

impl Session {
    /// A session with no wake-up: nothing is notified when the screen changes.
    /// Useful for tests and for a caller that polls [`Session::is_dirty`].
    pub fn new(size: TerminalSize) -> Self {
        Self::with_wake_up(size, || {})
    }

    /// A session that calls `wake_up` when the screen goes from clean to dirty.
    ///
    /// The callback runs on whichever thread made the change, with the lock
    /// released, and must not block: the frontend's version of it does nothing
    /// but `dispatch_async` a redraw onto the main queue.
    pub fn with_wake_up(size: TerminalSize, wake_up: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            shared: Arc::new(Shared {
                emulator: Mutex::new(Emulator {
                    screen: Screen::new(size),
                    parser: VtParser::new(),
                }),
                dirty: AtomicBool::new(false),
                running: AtomicBool::new(true),
                wake_up: Box::new(wake_up),
            }),
        }
    }

    /// Take the lock, recovering from poisoning rather than propagating it.
    ///
    /// A panic while the lock was held leaves the screen possibly inconsistent,
    /// but a terminal that stops drawing is worse than one drawing a damaged
    /// row, and a panic must never cross the FFI boundary (PRD §12).
    fn lock(&self) -> MutexGuard<'_, Emulator> {
        self.shared
            .emulator
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Feed PTY output through the parser, returning the bytes now owed to the
    /// program (PRD §9). The reader thread calls this and writes what comes
    /// back to the PTY.
    #[must_use = "these bytes are owed to the program: write them to the PTY (PRD §9)"]
    pub fn feed(&self, bytes: &[u8]) -> Vec<u8> {
        let replies = {
            let mut emulator = self.lock();
            let Emulator { screen, parser } = &mut *emulator;
            screen.advance(parser, bytes)
        };
        // Outside the lock: the callback may call back into this session.
        self.mark_dirty();
        replies
    }

    /// Copy the visible screen into `frame`. The UI thread calls this once per
    /// redraw, reusing one frame so the steady state allocates nothing.
    ///
    /// The dirty flag is cleared *before* the copy, so a write that lands
    /// mid-copy sets it again and earns one redundant redraw. Clearing it after
    /// would lose that write until something else happened to set the flag.
    pub fn render_into(&self, frame: &mut Frame) {
        self.shared.dirty.store(false, Ordering::Release);
        self.lock().screen.render_into(frame);
    }

    /// Whether the screen has changed since the last frame was taken.
    pub fn is_dirty(&self) -> bool {
        self.shared.dirty.load(Ordering::Acquire)
    }

    /// Read the screen under the lock. Does not mark anything dirty.
    pub fn with_screen<R>(&self, f: impl FnOnce(&Screen) -> R) -> R {
        f(&self.lock().screen)
    }

    /// Mutate the screen under the lock, then mark it dirty and wake the UI.
    pub fn update<R>(&self, f: impl FnOnce(&mut Screen) -> R) -> R {
        let out = f(&mut self.lock().screen);
        self.mark_dirty();
        out
    }

    /// Encode a keystroke against the current modes (PRD §8). The bytes go to
    /// the PTY; nothing on the screen changes here.
    pub fn encode_key(&self, key: Key, mods: Modifiers) -> Vec<u8> {
        let modes = self.with_screen(|screen| screen.modes());
        crate::keys::encode_key(key, mods, modes)
    }

    /// Encode pasted text, bracketed if the program asked for that.
    pub fn encode_paste(&self, text: &str) -> Vec<u8> {
        let modes = self.with_screen(|screen| screen.modes());
        crate::keys::encode_paste(text, modes)
    }

    /// Resize the terminal, reflowing under the lock (PRD §16.4).
    pub fn resize(&self, size: TerminalSize) {
        self.update(|screen| screen.resize(size));
    }

    /// The window title the program has asked for, copied out under the lock.
    pub fn title(&self) -> String {
        self.with_screen(|screen| screen.title().to_string())
    }

    /// Stop the session: no further wake-up is fired, whatever happens to the
    /// screen afterwards.
    ///
    /// This is the first step of the ordered shutdown in PRD §7 — it is what
    /// lets the threads be signalled and joined before the handle is destroyed,
    /// with no callback in flight into memory that is going away. Idempotent.
    pub fn shutdown(&self) {
        self.shared.running.store(false, Ordering::Release);
    }

    /// Whether the session is still running (i.e. has not been shut down).
    pub fn is_running(&self) -> bool {
        self.shared.running.load(Ordering::Acquire)
    }

    /// Mark the screen dirty, waking the UI on the clean-to-dirty edge only.
    /// Must be called with the lock released.
    fn mark_dirty(&self) {
        if !self.shared.dirty.swap(true, Ordering::AcqRel) && self.is_running() {
            (self.shared.wake_up)();
        }
    }
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The wake-up callback is not printable, and the screen is far too big
        // to dump; report the state a reader of a log would want.
        f.debug_struct("Session")
            .field("dirty", &self.is_dirty())
            .field("running", &self.is_running())
            .field("handles", &Arc::strong_count(&self.shared))
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    fn counting_session(size: TerminalSize) -> (Session, Arc<AtomicUsize>) {
        let wakes = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&wakes);
        let session = Session::with_wake_up(size, move || {
            counter.fetch_add(1, Ordering::SeqCst);
        });
        (session, wakes)
    }

    fn wakes_of(counter: &AtomicUsize) -> usize {
        counter.load(Ordering::SeqCst)
    }

    #[test]
    fn feeding_shows_up_in_the_frame() {
        let session = Session::new(TerminalSize::new(3, 10));
        let _ = session.feed(b"hello");
        let mut frame = Frame::new();
        session.render_into(&mut frame);
        assert_eq!(frame.row_text(0), "hello");
    }

    #[test]
    fn a_reply_comes_back_from_feed() {
        let session = Session::new(TerminalSize::new(3, 10));
        assert_eq!(session.feed(b"\x1b[6n"), b"\x1b[1;1R");
    }

    #[test]
    fn a_burst_of_output_wakes_the_ui_once() {
        // The load-bearing detail of PRD §7: a thousand chunks must not enqueue
        // a thousand redraws.
        let (session, wakes) = counting_session(TerminalSize::new(24, 80));
        for _ in 0..1000 {
            let _ = session.feed(b"x");
        }
        assert_eq!(wakes_of(&wakes), 1);
        assert!(session.is_dirty());
    }

    #[test]
    fn taking_a_frame_re_arms_the_wake_up() {
        let (session, wakes) = counting_session(TerminalSize::new(24, 80));
        let mut frame = Frame::new();
        let _ = session.feed(b"a");
        session.render_into(&mut frame);
        assert!(!session.is_dirty(), "the frame is up to date");
        let _ = session.feed(b"b");
        assert_eq!(wakes_of(&wakes), 2, "the next burst wakes the UI again");
    }

    #[test]
    fn a_write_during_a_copy_is_never_lost() {
        // The flag is cleared before the copy, so a write that lands while the
        // copy is in flight leaves the session dirty rather than being missed.
        let (session, _wakes) = counting_session(TerminalSize::new(3, 10));
        let mut frame = Frame::new();
        let _ = session.feed(b"early");
        session.render_into(&mut frame);

        // Stand in for the interleaving: render_into has cleared the flag, and
        // the write lands before the copy finishes.
        session.shared.dirty.store(false, Ordering::Release);
        let _ = session.feed(b" late");
        assert!(session.is_dirty(), "the write is still owed a redraw");

        session.render_into(&mut frame);
        assert_eq!(frame.row_text(0), "early late");
    }

    #[test]
    fn an_update_marks_the_screen_dirty() {
        let (session, wakes) = counting_session(TerminalSize::new(3, 10));
        session.update(|screen| screen.print("typed"));
        assert!(session.is_dirty());
        assert_eq!(wakes_of(&wakes), 1);
        assert_eq!(session.with_screen(|s| s.cursor().col()), 5);
    }

    #[test]
    fn resizing_reflows_under_the_lock() {
        let session = Session::new(TerminalSize::new(2, 6));
        let _ = session.feed(b"abcdefgh");
        session.resize(TerminalSize::new(2, 4));
        let mut frame = Frame::new();
        session.render_into(&mut frame);
        assert_eq!(frame.row_text(0), "abcd");
        assert_eq!(frame.row_text(1), "efgh");
    }

    #[test]
    fn the_title_crosses_the_lock_as_an_owned_string() {
        let session = Session::new(TerminalSize::new(2, 8));
        let _ = session.feed(b"\x1b]2;my shell\x07");
        assert_eq!(session.title(), "my shell");
    }

    #[test]
    fn keys_are_encoded_against_the_modes_the_program_set() {
        let session = Session::new(TerminalSize::new(3, 10));
        assert_eq!(session.encode_key(Key::Up, Modifiers::NONE), b"\x1b[A");
        let _ = session.feed(b"\x1b[?1h"); // the program turns on DECCKM
        assert_eq!(session.encode_key(Key::Up, Modifiers::NONE), b"\x1bOA");

        assert_eq!(session.encode_paste("ls"), b"ls");
        let _ = session.feed(b"\x1b[?2004h");
        assert_eq!(session.encode_paste("ls"), b"\x1b[200~ls\x1b[201~");
    }

    #[test]
    fn nothing_is_woken_after_shutdown() {
        // Step one of the ordered shutdown: no callback may be in flight into
        // memory the frontend is about to free.
        let (session, wakes) = counting_session(TerminalSize::new(3, 10));
        session.shutdown();
        assert!(!session.is_running());
        for _ in 0..10 {
            let _ = session.feed(b"x");
        }
        assert_eq!(wakes_of(&wakes), 0);
        assert!(session.is_dirty(), "the screen still changed, though");
        session.shutdown();
        assert!(!session.is_running(), "shutdown is idempotent");
    }

    #[test]
    fn clones_share_one_terminal() {
        let session = Session::new(TerminalSize::new(3, 10));
        let reader = session.clone();
        let _ = reader.feed(b"shared");
        assert_eq!(session.with_screen(|s| s.render().row_text(0)), "shared");
        reader.shutdown();
        assert!(!session.is_running(), "and one shutdown");
    }

    #[test]
    fn a_reader_thread_and_a_ui_thread_share_it_safely() {
        let (session, wakes) = counting_session(TerminalSize::new(24, 80));
        let reader = session.clone();
        let writer = std::thread::spawn(move || {
            for _ in 0..200 {
                let _ = reader.feed(b"line of output\r\n");
            }
        });

        let mut frame = Frame::new();
        let mut frames = 0;
        while !writer.is_finished() {
            session.render_into(&mut frame);
            frames += 1;
            std::thread::yield_now();
        }
        writer.join().expect("the reader thread must not panic");

        session.render_into(&mut frame);
        assert_eq!(frame.size(), TerminalSize::new(24, 80));
        assert!(frames > 0);
        // Coalescing means far fewer wake-ups than feeds, whatever the
        // interleaving turned out to be.
        assert!(
            wakes_of(&wakes) <= frames + 1,
            "one wake-up per redraw at most, not one per chunk"
        );
    }

    #[test]
    fn a_panic_holding_the_lock_does_not_stop_the_terminal() {
        // PRD §12: a panic must never cross the boundary. A poisoned lock is
        // recovered rather than propagated -- a terminal drawing a damaged row
        // beats a terminal that stops drawing.
        let session = Session::new(TerminalSize::new(3, 10));
        let victim = session.clone();
        let panicked = std::thread::spawn(move || {
            victim.update(|screen| {
                screen.print("half");
                panic!("something went wrong mid-update");
            });
        })
        .join();
        assert!(panicked.is_err(), "the panicking thread did panic");

        let _ = session.feed(b" done");
        let mut frame = Frame::new();
        session.render_into(&mut frame);
        assert_eq!(frame.row_text(0), "half done");
    }
}
