//! Cell-grid geometry: screen dimensions and display positions.
//!
//! These are *display* coordinates — `(row, col)` in the visible viewport, the
//! unstable-across-resize half of the two coordinate systems in PRD §16.2. The
//! stable *logical* coordinates (`line_id`, `char_offset`) are a separate type
//! that arrives with the buffer model; they are deliberately not modelled here.

/// The dimensions of a terminal screen, in character cells.
///
/// This is one of the plain-data types that crosses the FFI boundary by value
/// (PRD §4.2), so it is `#[repr(C)]` with fixed-width fields rather than
/// `usize`, keeping its layout independent of the target's pointer width.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct TerminalSize {
    pub rows: u16,
    pub cols: u16,
}

impl TerminalSize {
    pub const fn new(rows: u16, cols: u16) -> Self {
        Self { rows, cols }
    }

    /// Total number of cells in a screen of this size.
    ///
    /// Returns `usize` because it is used for allocation sizing on the Rust
    /// side; the boundary never sees this value.
    pub const fn cell_count(self) -> usize {
        self.rows as usize * self.cols as usize
    }

    /// A screen with no cells to draw — zero rows or zero columns. Resizes can
    /// legitimately pass through this state (e.g. a window collapsed to a
    /// sliver), so engine code must tolerate it rather than assume it away.
    pub const fn is_empty(self) -> bool {
        self.rows == 0 || self.cols == 0
    }
}

/// A position within the visible grid, in display coordinates.
///
/// `(row, col)` are zero-based from the top-left of the viewport. Because these
/// are display coordinates they are *not* stable across a reflow (PRD §16.2);
/// anything that must survive a resize is anchored logically instead.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
pub struct Position {
    pub row: u16,
    pub col: u16,
}

impl Position {
    pub const fn new(row: u16, col: u16) -> Self {
        Self { row, col }
    }

    /// Whether this position falls within a screen of the given size.
    ///
    /// A position is in bounds when its row is above `size.rows` and its column
    /// is left of `size.cols` — the half-open `[0, n)` convention.
    pub const fn is_within(self, size: TerminalSize) -> bool {
        self.row < size.rows && self.col < size.cols
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cell_count_multiplies_without_overflow_at_typical_sizes() {
        assert_eq!(TerminalSize::new(24, 80).cell_count(), 1920);
        // A large-but-plausible screen stays well inside usize.
        assert_eq!(TerminalSize::new(1000, 1000).cell_count(), 1_000_000);
    }

    #[test]
    fn zero_dimension_is_empty() {
        assert!(TerminalSize::new(0, 80).is_empty());
        assert!(TerminalSize::new(24, 0).is_empty());
        assert!(TerminalSize::new(0, 0).is_empty());
        assert!(!TerminalSize::new(1, 1).is_empty());
    }

    #[test]
    fn position_bounds_are_half_open() {
        let size = TerminalSize::new(24, 80);
        assert!(Position::new(0, 0).is_within(size));
        assert!(Position::new(23, 79).is_within(size));
        // Row and column equal to the extent are out of bounds.
        assert!(!Position::new(24, 79).is_within(size));
        assert!(!Position::new(23, 80).is_within(size));
    }

    #[test]
    fn nothing_is_within_an_empty_screen() {
        assert!(!Position::new(0, 0).is_within(TerminalSize::new(0, 0)));
    }
}
