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
//! This module tree currently holds only the foundational value types. The
//! buffer model (logical lines, reflow), the VT parser, the PTY and the FFI
//! surface are built on top of these in later increments.

pub mod cell;
pub mod color;
pub mod cursor;
pub mod geometry;
pub mod grid;
pub mod logical_line;
pub mod text;

pub use cell::{Cell, CellAttrs};
pub use color::Color;
pub use cursor::Cursor;
pub use geometry::{Position, TerminalSize};
pub use grid::Grid;
pub use logical_line::{AttrRun, LineId, LogicalLine};
