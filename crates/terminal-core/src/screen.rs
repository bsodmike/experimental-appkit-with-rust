//! The primary screen: the mutable active grid plus its scrollback.
//!
//! PRD §17.5 #21: a dedicated type for the primary buffer, distinct from the
//! dumb [`Grid`](crate::grid::Grid) that backs the alternate screen. The active
//! grid is the mutation surface where the cursor writes and scroll regions
//! operate (the hybrid model, #16); each row carries the identity of the logical
//! line it belongs to (#17) so on-screen anchors survive reflow, and scrollback
//! holds the frozen logical lines above the screen.
//!
//! This module currently holds the container and its accessors. The write path
//! (grapheme placement, cursor advance, autowrap), scrolling with freeze into
//! scrollback, and reflow are built on top of it in later increments.

use std::collections::VecDeque;

use crate::cell::Cell;
use crate::cursor::Cursor;
use crate::geometry::{Position, TerminalSize};
use crate::logical_line::LineId;
use crate::scrollback::Scrollback;

/// One display row of the active grid: a full-width line of cells plus the
/// logical line it belongs to and whether it soft-wraps into the next row.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Row {
    cells: Vec<Cell>,
    line_id: LineId,
    /// True when this row continues into the next by a soft wrap (autowrap),
    /// false when it ends at a hard newline. This is the per-row wrap state that
    /// reflow reads to reconstruct logical lines (#17); a logical line is a run
    /// of consecutive rows sharing `line_id`.
    wrapped: bool,
}

impl Row {
    /// A blank row of `cols` cells belonging to `line_id`.
    pub fn blank(cols: u16, line_id: LineId) -> Self {
        Self {
            cells: vec![Cell::blank(); cols as usize],
            line_id,
            wrapped: false,
        }
    }

    pub fn cells(&self) -> &[Cell] {
        &self.cells
    }

    pub fn cells_mut(&mut self) -> &mut [Cell] {
        &mut self.cells
    }

    pub fn line_id(&self) -> LineId {
        self.line_id
    }

    pub fn set_line_id(&mut self, id: LineId) {
        self.line_id = id;
    }

    pub fn is_wrapped(&self) -> bool {
        self.wrapped
    }

    pub fn set_wrapped(&mut self, wrapped: bool) {
        self.wrapped = wrapped;
    }

    pub fn width(&self) -> u16 {
        self.cells.len() as u16
    }
}

/// The primary screen: active rows, cursor, and scrollback.
#[derive(Clone, Debug)]
pub struct Screen {
    size: TerminalSize,
    rows: VecDeque<Row>,
    cursor: Cursor,
    scrollback: Scrollback,
    next_line_id: u64,
}

impl Screen {
    /// A fresh screen of `size`, every row blank. Each initial row is its own
    /// logical line (distinct `line_id`), so an empty terminal is `rows` empty
    /// lines rather than one — which is what scrolling and reflow expect.
    pub fn new(size: TerminalSize) -> Self {
        Self::with_scrollback(size, Scrollback::with_defaults())
    }

    pub fn with_scrollback(size: TerminalSize, scrollback: Scrollback) -> Self {
        let mut next_line_id = 0u64;
        let mut rows = VecDeque::with_capacity(size.rows as usize);
        for _ in 0..size.rows {
            rows.push_back(Row::blank(size.cols, LineId(next_line_id)));
            next_line_id += 1;
        }
        Self {
            size,
            rows,
            cursor: Cursor::new(),
            scrollback,
            next_line_id,
        }
    }

    /// Allocate the next monotonic line id (#12). Never reused, so eviction of an
    /// older id leaves stored anchors detectably stale rather than aliased.
    // Consumed by the write/scroll path in the next increment; staged here with
    // the container it belongs to.
    #[allow(dead_code)]
    pub(crate) fn alloc_line_id(&mut self) -> LineId {
        let id = LineId(self.next_line_id);
        self.next_line_id += 1;
        id
    }

    pub fn size(&self) -> TerminalSize {
        self.size
    }

    pub fn cursor(&self) -> &Cursor {
        &self.cursor
    }

    pub fn cursor_mut(&mut self) -> &mut Cursor {
        &mut self.cursor
    }

    pub fn scrollback(&self) -> &Scrollback {
        &self.scrollback
    }

    pub fn scrollback_mut(&mut self) -> &mut Scrollback {
        &mut self.scrollback
    }

    pub fn row(&self, row: u16) -> Option<&Row> {
        self.rows.get(row as usize)
    }

    pub fn row_mut(&mut self, row: u16) -> Option<&mut Row> {
        self.rows.get_mut(row as usize)
    }

    /// The active rows, top to bottom.
    pub fn rows(&self) -> impl Iterator<Item = &Row> {
        self.rows.iter()
    }

    pub fn cell(&self, pos: Position) -> Option<&Cell> {
        if !pos.is_within(self.size) {
            return None;
        }
        self.rows.get(pos.row as usize)?.cells.get(pos.col as usize)
    }

    pub fn cell_mut(&mut self, pos: Position) -> Option<&mut Cell> {
        if !pos.is_within(self.size) {
            return None;
        }
        self.rows
            .get_mut(pos.row as usize)?
            .cells
            .get_mut(pos.col as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_screen_is_sized_blank_with_cursor_at_origin() {
        let s = Screen::new(TerminalSize::new(3, 4));
        assert_eq!(s.size(), TerminalSize::new(3, 4));
        assert_eq!(s.cursor().position(), Position::new(0, 0));
        assert!(s.scrollback().is_empty());
        assert_eq!(s.rows().count(), 3);
        assert!(s.rows().all(|r| r.cells().iter().all(Cell::is_blank)));
        assert!(s.rows().all(|r| r.width() == 4));
        assert!(s.rows().all(|r| !r.is_wrapped()));
    }

    #[test]
    fn each_initial_row_is_its_own_logical_line() {
        let s = Screen::new(TerminalSize::new(3, 4));
        let ids: Vec<u64> = s.rows().map(|r| r.line_id().0).collect();
        assert_eq!(ids, [0, 1, 2]);
    }

    #[test]
    fn alloc_line_id_is_monotonic_and_starts_after_initial_rows() {
        let mut s = Screen::new(TerminalSize::new(3, 4));
        // Rows 0..3 consumed ids 0..3, so the next is 3.
        assert_eq!(s.alloc_line_id(), LineId(3));
        assert_eq!(s.alloc_line_id(), LineId(4));
    }

    #[test]
    fn cell_access_respects_bounds() {
        let mut s = Screen::new(TerminalSize::new(2, 2));
        assert!(s.cell(Position::new(1, 1)).is_some());
        assert!(s.cell(Position::new(2, 0)).is_none());
        assert!(s.cell(Position::new(0, 2)).is_none());

        s.cell_mut(Position::new(0, 0)).unwrap().content = "x".into();
        assert_eq!(s.cell(Position::new(0, 0)).unwrap().content, "x");
    }

    #[test]
    fn row_metadata_is_editable() {
        let mut s = Screen::new(TerminalSize::new(2, 2));
        let r = s.row_mut(0).unwrap();
        r.set_wrapped(true);
        assert!(s.row(0).unwrap().is_wrapped());
    }
}
