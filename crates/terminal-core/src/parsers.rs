//! Parsers that turn raw byte streams into engine commands.
//!
//! Currently the VT/ANSI parser ([`vt`]). Parsers here are pure — they produce
//! typed commands and touch no `Screen` — so they are testable purely as bytes
//! in, commands out.

pub mod vt;
