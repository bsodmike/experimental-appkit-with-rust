//! # terminal-pty
//!
//! The pseudo-terminal and the child process on the far end of it.
//!
//! PRD §5 puts the PTY and the shell process on the Rust side of the boundary —
//! they are POSIX, not AppKit — but they are still *platform* code, so they live
//! here rather than in `terminal-core`, which stays pure and testable with
//! nothing but a compiler (PRD §19, crate layout).
//!
//! What this crate owns is the file descriptor and the process: opening a pty,
//! spawning a shell as the leader of its own session with that pty as its
//! controlling terminal, reading and writing bytes, telling the kernel the
//! window size, and stopping all of it in an order that leaves nothing running
//! (PRD §7, shutdown).

mod interrupt;
mod pty;
mod terminal;

pub use interrupt::Interrupt;
pub use pty::{Pty, PtyHandle};
pub use terminal::Terminal;
