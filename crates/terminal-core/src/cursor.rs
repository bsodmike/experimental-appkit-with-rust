//! The cursor: where the next glyph lands, plus the deferred-wrap flag.

use crate::geometry::{Position, TerminalSize};

/// The terminal cursor.
///
/// It holds only what is intrinsically the cursor's own state: its position in
/// display coordinates, and the *pending-wrap* flag. Notably it does **not**
/// carry the current SGR attributes or the selected charset — those live with
/// the emulator's mode state, not here.
///
/// Because `Cursor` is `Copy`, the emulator implements DECSC / DECRC (save and
/// restore cursor) by snapshotting the whole value alongside the SGR and
/// charset it also has to save. There is deliberately no `save`/`restore`
/// method on the cursor itself, because the cursor does not own those other
/// pieces and should not pretend to.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Cursor {
    position: Position,
    /// The deferred end-of-line wrap, a.k.a. the "last column" flag.
    ///
    /// When a glyph is written into the final column the cursor does *not*
    /// immediately move to the next line; it stays on the last column and this
    /// flag is armed. Only the next printable glyph performs the wrap. This is
    /// the classic VT100 behaviour, and getting it right is what stops a line
    /// that exactly fills the width from wrapping one row too early.
    pending_wrap: bool,
}

impl Cursor {
    /// A new cursor at the origin `(0, 0)` with no pending wrap.
    pub fn new() -> Self {
        Self::default()
    }

    pub fn position(&self) -> Position {
        self.position
    }

    pub fn row(&self) -> u16 {
        self.position.row
    }

    pub fn col(&self) -> u16 {
        self.position.col
    }

    /// Move the cursor to `pos`, cancelling any pending wrap.
    ///
    /// An explicit cursor movement always clears the deferred-wrap flag: only
    /// *writing a glyph* at the armed position performs the wrap, so moving the
    /// cursor away disarms it. Note this does not clamp to any screen — bounds
    /// enforcement is the caller's job (see [`Cursor::clamp_to`]), because only
    /// the emulator knows the size and the active scroll margins.
    pub fn move_to(&mut self, pos: Position) {
        self.position = pos;
        self.pending_wrap = false;
    }

    pub fn pending_wrap(&self) -> bool {
        self.pending_wrap
    }

    /// Arm the deferred wrap. Called after a glyph is written into the last
    /// column: the cursor stays put and the *next* glyph will wrap first.
    pub fn arm_wrap(&mut self) {
        self.pending_wrap = true;
    }

    /// Consume the deferred-wrap flag, returning whether it was set and
    /// clearing it. The writer calls this before placing a glyph; a `true`
    /// result means "wrap to the next line before writing."
    pub fn take_pending_wrap(&mut self) -> bool {
        std::mem::take(&mut self.pending_wrap)
    }

    /// Clamp the cursor within a screen of `size`, saturating toward the
    /// bottom-right edge, and clear any pending wrap.
    ///
    /// Used after a resize shrinks the grid, so the cursor never points outside
    /// the cells that exist. The pending wrap is cleared because the last
    /// column it referred to may no longer be there.
    pub fn clamp_to(&mut self, size: TerminalSize) {
        // saturating_sub keeps a zero-sized dimension pinned at index 0.
        self.position.row = self.position.row.min(size.rows.saturating_sub(1));
        self.position.col = self.position.col.min(size.cols.saturating_sub(1));
        self.pending_wrap = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_cursor_is_at_origin_without_pending_wrap() {
        let c = Cursor::new();
        assert_eq!(c.position(), Position::new(0, 0));
        assert_eq!((c.row(), c.col()), (0, 0));
        assert!(!c.pending_wrap());
    }

    #[test]
    fn move_to_sets_position_and_clears_pending_wrap() {
        let mut c = Cursor::new();
        c.arm_wrap();
        c.move_to(Position::new(5, 9));
        assert_eq!(c.position(), Position::new(5, 9));
        assert!(
            !c.pending_wrap(),
            "moving the cursor disarms the deferred wrap"
        );
    }

    #[test]
    fn take_pending_wrap_reports_then_clears() {
        let mut c = Cursor::new();
        assert!(!c.take_pending_wrap(), "starts disarmed");
        c.arm_wrap();
        assert!(c.pending_wrap());
        assert!(c.take_pending_wrap(), "first take sees it armed");
        assert!(!c.take_pending_wrap(), "second take sees it cleared");
    }

    #[test]
    fn clamp_pulls_the_cursor_inside_a_shrunk_screen() {
        let mut c = Cursor::new();
        c.move_to(Position::new(40, 100));
        c.arm_wrap();
        c.clamp_to(TerminalSize::new(24, 80));
        assert_eq!(c.position(), Position::new(23, 79));
        assert!(
            !c.pending_wrap(),
            "clamping clears a wrap on a column that may be gone"
        );
    }

    #[test]
    fn clamp_is_a_noop_when_already_in_bounds() {
        let mut c = Cursor::new();
        c.move_to(Position::new(10, 20));
        c.clamp_to(TerminalSize::new(24, 80));
        assert_eq!(c.position(), Position::new(10, 20));
    }

    #[test]
    fn clamp_pins_to_origin_on_an_empty_screen() {
        let mut c = Cursor::new();
        c.move_to(Position::new(3, 7));
        c.clamp_to(TerminalSize::new(0, 0));
        assert_eq!(c.position(), Position::new(0, 0));
    }

    #[test]
    fn copy_snapshot_round_trips_like_decsc_decrc() {
        // The emulator saves the cursor for DECSC by copying it, mutates, then
        // restores with DECRC by assigning the copy back. Both position and the
        // pending-wrap flag must survive the round trip.
        let mut c = Cursor::new();
        c.move_to(Position::new(2, 3));
        c.arm_wrap();

        let saved = c; // DECSC
        c.move_to(Position::new(0, 0));
        assert_ne!(c, saved);

        c = saved; // DECRC
        assert_eq!(c.position(), Position::new(2, 3));
        assert!(c.pending_wrap());
    }
}
