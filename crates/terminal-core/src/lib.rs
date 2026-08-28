//! # terminal-core
//!
//! The terminal *engine*: the rich Rust half of the hourglass described in
//! `PRD.md` §2. It owns terminal semantics — the grid, cursor, attributes,
//! modes, scrollback, selection, VT parsing, PTY and key encoding — and knows
//! nothing about AppKit, Core Text, or macOS.
//!
//! The guiding test (PRD §18): *if the engine ever needs a Mac in order to be
//! tested, something has leaked across the boundary.* Everything in this crate
//! is therefore pure Rust with no platform dependency, and everything is
//! unit-testable headlessly.
//!
//! The public API is the [`prelude`] and nothing else: the implementation
//! modules are private, so their internal layout can change without breaking
//! consumers (see `docs/adrs/2026-08-28.adr-private-modules-and-prelude.md`).
//! The buffer model (logical lines, reflow), the VT parser, the PTY and the FFI
//! surface build on top of these modules.

mod cell;
mod color;
mod cursor;
mod geometry;
mod grid;
mod keys;
mod logical_line;
mod parsers;
mod render;
mod screen;
mod scrollback;
mod session;
mod text;

/// The crate's public surface. Import it with `use terminal_core::prelude::*;`.
///
/// Every type the engine exposes is re-exported here and nowhere else. Keeping
/// the implementation modules private means their internals — field layouts,
/// helper functions, module boundaries — can be refactored freely without
/// changing what consumers depend on.
pub mod prelude {
    pub use crate::cell::{Cell, CellAttrs};
    pub use crate::color::Color;
    pub use crate::cursor::Cursor;
    pub use crate::geometry::{Position, TerminalSize};
    pub use crate::grid::Grid;
    pub use crate::keys::{Key, Keypad, Modifiers, encode_key, encode_paste};
    pub use crate::logical_line::{AttrRun, LineId, LogicalLine};
    pub use crate::render::{Frame, Run};
    pub use crate::screen::{Modes, Pen, Row, Screen};
    pub use crate::scrollback::Scrollback;
    pub use crate::session::Session;

    /// The byte-stream parsers, reachable as `prelude::parsers::vt`.
    pub mod parsers {
        pub mod vt {
            pub use crate::parsers::vt::{Command, EraseMode, Mode, Sgr, VtParser};
        }
    }
}
