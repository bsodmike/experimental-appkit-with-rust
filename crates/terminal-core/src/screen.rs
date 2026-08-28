//! The primary screen: the mutable active grid plus its scrollback.
//!
//! PRD §17.5 #21: a dedicated type for the primary buffer, distinct from the
//! dumb [`Grid`](crate::grid::Grid) that backs the alternate screen. The active
//! grid is the mutation surface where the cursor writes and scroll regions
//! operate (the hybrid model, #16); each row carries the identity of the logical
//! line it belongs to (#17) so on-screen anchors survive reflow, and scrollback
//! holds the frozen logical lines above the screen.
//!
//! This module holds the container, the printable-text write path (grapheme
//! placement, cursor advance, deferred autowrap, wide characters) and line
//! movement (`carriage_return`, `line_feed`) including scrolling the top row off
//! into scrollback. Reflow is built on top of this in a later increment.

use std::collections::VecDeque;

use compact_str::CompactString;

use crate::cell::{Cell, CellAttrs};
use crate::color::Color;
use crate::cursor::Cursor;
use crate::geometry::{Position, TerminalSize};
use crate::logical_line::{LineId, LogicalLine};
use crate::scrollback::Scrollback;
use crate::text::{display_width, grapheme_width, graphemes};

/// The current drawing style applied to newly written cells (the SGR "pen").
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Pen {
    pub fg: Color,
    pub bg: Color,
    pub attrs: CellAttrs,
}

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

    /// A row built from ready-made cells (used by reflow when re-laying display
    /// rows produced by the renderer).
    pub fn with_cells(cells: Vec<Cell>, line_id: LineId, wrapped: bool) -> Self {
        Self {
            cells,
            line_id,
            wrapped,
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

/// The primary screen: active rows, cursor, scrollback and the current pen.
#[derive(Clone, Debug)]
pub struct Screen {
    size: TerminalSize,
    rows: VecDeque<Row>,
    cursor: Cursor,
    scrollback: Scrollback,
    pen: Pen,
    next_line_id: u64,
    /// The frozen head of the one logical line currently straddling the
    /// scrollback/active boundary (#24): its earlier rows have scrolled off but
    /// its last row has not ended yet, so it is not a complete scrollback line.
    /// `None` between logical lines.
    pending: Option<LogicalLine>,
}

impl Screen {
    /// A fresh screen of `size`, every row blank. Each initial row is its own
    /// logical line (distinct `line_id`), so an empty terminal is `rows` empty
    /// lines rather than one, which is what scrolling and reflow expect.
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
            pen: Pen::default(),
            next_line_id,
            pending: None,
        }
    }

    /// Allocate the next monotonic line id (#12). Never reused, so eviction of an
    /// older id leaves stored anchors detectably stale rather than aliased.
    fn alloc_line_id(&mut self) -> LineId {
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

    pub fn pen(&self) -> Pen {
        self.pen
    }

    pub fn pen_mut(&mut self) -> &mut Pen {
        &mut self.pen
    }

    pub fn scrollback(&self) -> &Scrollback {
        &self.scrollback
    }

    pub fn scrollback_mut(&mut self) -> &mut Scrollback {
        &mut self.scrollback
    }

    /// The frozen head of the currently-straddling logical line, if any (#24).
    /// Reflow and anchor resolution concatenate this with the active tail.
    pub fn pending_head(&self) -> Option<&LogicalLine> {
        self.pending.as_ref()
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

    /// Write printable text at the cursor, one grapheme cluster per cell,
    /// advancing the cursor and wrapping as needed. Control characters are not
    /// handled here; the parser calls [`Screen::line_feed`] and
    /// [`Screen::carriage_return`] for those.
    pub fn print(&mut self, text: &str) {
        if self.size.is_empty() {
            return;
        }
        let cols = self.size.cols;
        for cluster in graphemes(text) {
            let w = grapheme_width(cluster);
            if w == 0 {
                self.attach_combining(cluster);
                continue;
            }
            // Realise a deferred wrap, or wrap early for a wide glyph that will
            // not fit in the columns left (PRD §16.3 leaves a padding column).
            if self.cursor.pending_wrap() || self.cursor.col() as u32 + w as u32 > cols as u32 {
                self.autowrap();
            }
            let row = self.cursor.row();
            let col = self.cursor.col();
            self.put(row, col, cluster, w);
            let new_col = col + w;
            if new_col >= cols {
                // At the right edge: sit on the last column and defer the wrap.
                self.cursor.move_to(Position::new(row, cols - 1));
                self.cursor.arm_wrap();
            } else {
                self.cursor.move_to(Position::new(row, new_col));
            }
        }
    }

    /// Carriage return: move the cursor to column 0 of its current row.
    pub fn carriage_return(&mut self) {
        let row = self.cursor.row();
        self.cursor.move_to(Position::new(row, 0));
    }

    /// Line feed: move the cursor down one row, scrolling the screen (freezing
    /// the top row into scrollback) when already on the last row. The column is
    /// preserved; a carriage return is a separate control.
    pub fn line_feed(&mut self) {
        let row = self.cursor.row();
        let col = self.cursor.col();
        if (row as usize + 1) < self.rows.len() {
            self.cursor.move_to(Position::new(row + 1, col));
        } else {
            let id = self.alloc_line_id();
            self.scroll_up(Row::blank(self.size.cols, id));
            self.cursor
                .move_to(Position::new(self.size.rows.saturating_sub(1), col));
        }
    }

    /// Soft-wrap the current row into the next: mark it wrapped and move to the
    /// start of the following row, which becomes a continuation of the same
    /// logical line. Scrolls if the current row is the last.
    fn autowrap(&mut self) {
        let row = self.cursor.row();
        let line_id = self.rows.get(row as usize).map(Row::line_id);
        if let Some(r) = self.rows.get_mut(row as usize) {
            r.set_wrapped(true);
        }
        let Some(line_id) = line_id else { return };
        if (row as usize + 1) < self.rows.len() {
            if let Some(next) = self.rows.get_mut(row as usize + 1) {
                next.set_line_id(line_id);
                next.set_wrapped(false);
            }
            self.cursor.move_to(Position::new(row + 1, 0));
        } else {
            self.scroll_up(Row::blank(self.size.cols, line_id));
            self.cursor
                .move_to(Position::new(self.size.rows.saturating_sub(1), 0));
        }
    }

    /// Remove the top row, freezing it into scrollback, and append `new_bottom`.
    fn scroll_up(&mut self, new_bottom: Row) {
        if let Some(top) = self.rows.pop_front() {
            self.freeze_row(top);
        }
        self.rows.push_back(new_bottom);
    }

    /// Fold a scrolled-off row into scrollback. Consecutive rows of one logical
    /// line accumulate in `pending` and are sealed into a complete scrollback
    /// line when the line's final (non-wrapped) row scrolls off (#24).
    fn freeze_row(&mut self, row: Row) {
        let ends_line = !row.is_wrapped();
        let continues = matches!(&self.pending, Some(p) if p.id() == row.line_id());
        if continues {
            self.pending
                .as_mut()
                .unwrap()
                .push_cells(row.cells(), ends_line);
        } else {
            if let Some(done) = self.pending.take() {
                self.scrollback.push(done);
            }
            let mut line = LogicalLine::new(row.line_id());
            line.push_cells(row.cells(), ends_line);
            self.pending = Some(line);
        }
        if ends_line && let Some(done) = self.pending.take() {
            self.scrollback.push(done);
        }
    }

    /// Place a grapheme (and, if wide, its trailing spacer) at `(row, col)` with
    /// the current pen.
    fn put(&mut self, row: u16, col: u16, content: &str, w: u16) {
        let Pen { fg, bg, attrs } = self.pen;
        let Some(r) = self.rows.get_mut(row as usize) else {
            return;
        };
        let cells = r.cells_mut();
        let ci = col as usize;
        if ci >= cells.len() {
            return;
        }
        cells[ci] = Cell {
            content: content.into(),
            fg,
            bg,
            attrs,
        };
        if w == 2
            && let Some(spacer) = cells.get_mut(ci + 1)
        {
            // The spacer carries empty content; it renders as part of the wide
            // glyph and contributes no bytes when the line is packed.
            *spacer = Cell {
                content: CompactString::const_new(""),
                fg,
                bg,
                attrs,
            };
        }
    }

    /// Attach a zero-width (combining) cluster to the last written cell.
    fn attach_combining(&mut self, cluster: &str) {
        let row = self.cursor.row();
        let col = self.cursor.col();
        let target = if self.cursor.pending_wrap() {
            Some(col)
        } else if col > 0 {
            Some(col - 1)
        } else {
            None
        };
        if let Some(tc) = target
            && let Some(r) = self.rows.get_mut(row as usize)
            && let Some(cell) = r.cells_mut().get_mut(tc as usize)
            && !cell.content.is_empty()
        {
            cell.content.push_str(cluster);
        }
    }

    /// Resize the screen, reflowing wrapped lines to the new width (PRD §16).
    ///
    /// The active logical lines (grid rows regrouped by `line_id`, with the
    /// straddling head folded back in) are re-wrapped to the new width; display
    /// rows beyond the new height freeze into scrollback, and the cursor is
    /// re-derived from its logical position so it stays on the same text.
    ///
    /// Limitation for now: growing the height pads with blank rows at the bottom
    /// rather than pulling lines back from scrollback; the viewport/scroll anchor
    /// (§16.3) that would do so is a later increment.
    pub fn resize(&mut self, new_size: TerminalSize) {
        if new_size == self.size {
            return;
        }
        if new_size.is_empty() {
            self.resize_to_empty(new_size);
            return;
        }

        let (anchor_id, anchor_off) = self.cursor_logical_anchor();
        let lines = self.reconstruct_active_lines();
        self.size = new_size;
        let new_cols = new_size.cols;
        let new_rows = new_size.rows as usize;

        let mut flat: Vec<Row> = Vec::new();
        let mut cursor_at: Option<(usize, u16, bool)> = None;
        for mut line in lines {
            let lid = line.id();
            let first = flat.len();
            if lid == anchor_id {
                let off = anchor_off.min(line.byte_len());
                let starts = line.wrap(new_cols).to_vec();
                let sub = starts.partition_point(|&s| s <= off);
                let row_start = if sub == 0 {
                    0
                } else {
                    starts[sub - 1] as usize
                };
                let width = display_width(&line.text()[row_start..off as usize]);
                let arm = width as u16 >= new_cols;
                let col = if arm {
                    new_cols.saturating_sub(1)
                } else {
                    width as u16
                };
                cursor_at = Some((first + sub, col, arm));
            }
            let rendered = line.render_rows(new_cols);
            let last = rendered.len().saturating_sub(1);
            for (k, cells) in rendered.into_iter().enumerate() {
                flat.push(Row::with_cells(cells, lid, k < last));
            }
        }

        let total = flat.len();
        let overflow = total.saturating_sub(new_rows);
        let mut iter = flat.into_iter();
        for _ in 0..overflow {
            let row = iter.next().unwrap();
            self.freeze_row(row);
        }
        let mut rows: VecDeque<Row> = iter.collect();
        while rows.len() < new_rows {
            let id = self.alloc_line_id();
            rows.push_back(Row::blank(new_cols, id));
        }
        self.rows = rows;

        let (flat_row, col, arm) = cursor_at.unwrap_or((0, 0, false));
        let row = flat_row
            .saturating_sub(overflow)
            .min(new_rows.saturating_sub(1)) as u16;
        self.cursor.move_to(Position::new(row, col));
        if arm {
            self.cursor.arm_wrap();
        }
    }

    /// Degenerate resize to a zero-sized screen: preserve content by freezing all
    /// active lines into scrollback, then present blank rows.
    fn resize_to_empty(&mut self, new_size: TerminalSize) {
        for line in self.reconstruct_active_lines() {
            self.scrollback.push(line);
        }
        self.size = new_size;
        let mut rows = VecDeque::new();
        for _ in 0..new_size.rows {
            let id = self.alloc_line_id();
            rows.push_back(Row::blank(new_size.cols, id));
        }
        self.rows = rows;
        self.cursor.move_to(Position::new(0, 0));
    }

    /// The cursor's position as a logical anchor: the id of the logical line it
    /// sits on, and the byte offset into that line's full text (including any
    /// frozen head in `pending`). Stable across reflow (#20, §16.2).
    fn cursor_logical_anchor(&self) -> (LineId, u32) {
        let r = self.cursor.row() as usize;
        let c = self.cursor.col() as usize;
        let Some(row_r) = self.rows.get(r) else {
            return (LineId(0), 0);
        };
        let lid = row_r.line_id();
        let mut r0 = r;
        while r0 > 0 && self.rows[r0 - 1].line_id() == lid {
            r0 -= 1;
        }
        let mut offset = 0u32;
        if let Some(p) = &self.pending
            && p.id() == lid
        {
            offset += p.byte_len();
        }
        for rr in r0..r {
            offset += row_content_bytes(&self.rows[rr], self.rows[rr].width() as usize);
        }
        offset += row_content_bytes(&self.rows[r], c);
        (lid, offset)
    }

    /// Regroup the active grid rows into logical lines (consecutive rows sharing
    /// a `line_id`), folding the straddling head from `pending` into the first
    /// line if it continues it. Consumes `pending`.
    fn reconstruct_active_lines(&mut self) -> Vec<LogicalLine> {
        let pending = self.pending.take();
        let mut lines: Vec<LogicalLine> = Vec::new();
        let mut i = 0;
        while i < self.rows.len() {
            let lid = self.rows[i].line_id();
            let mut line =
                if lines.is_empty() && i == 0 && matches!(&pending, Some(p) if p.id() == lid) {
                    pending.clone().unwrap()
                } else {
                    LogicalLine::new(lid)
                };
            while i < self.rows.len() && self.rows[i].line_id() == lid {
                let ends = !self.rows[i].is_wrapped();
                line.push_cells(self.rows[i].cells(), ends);
                i += 1;
            }
            lines.push(line);
        }
        lines
    }
}

/// The number of content bytes in the first `upto` cells of a row (blanks count
/// as their space byte, wide-char spacers as nothing).
fn row_content_bytes(row: &Row, upto: usize) -> u32 {
    let cells = row.cells();
    cells[..upto.min(cells.len())]
        .iter()
        .map(|c| c.content.len() as u32)
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn content_at(s: &Screen, row: u16, col: u16) -> String {
        s.cell(Position::new(row, col)).unwrap().content.to_string()
    }

    #[test]
    fn new_screen_is_sized_blank_with_cursor_at_origin() {
        let s = Screen::new(TerminalSize::new(3, 4));
        assert_eq!(s.size(), TerminalSize::new(3, 4));
        assert_eq!(s.cursor().position(), Position::new(0, 0));
        assert!(s.scrollback().is_empty());
        assert_eq!(s.rows().count(), 3);
        assert!(s.rows().all(|r| r.cells().iter().all(Cell::is_blank)));
        assert!(s.rows().all(|r| r.width() == 4));
    }

    #[test]
    fn each_initial_row_is_its_own_logical_line() {
        let s = Screen::new(TerminalSize::new(3, 4));
        let ids: Vec<u64> = s.rows().map(|r| r.line_id().0).collect();
        assert_eq!(ids, [0, 1, 2]);
    }

    #[test]
    fn cell_access_respects_bounds() {
        let mut s = Screen::new(TerminalSize::new(2, 2));
        assert!(s.cell(Position::new(1, 1)).is_some());
        assert!(s.cell(Position::new(2, 0)).is_none());
        s.cell_mut(Position::new(0, 0)).unwrap().content = "x".into();
        assert_eq!(content_at(&s, 0, 0), "x");
    }

    #[test]
    fn print_lays_graphemes_and_advances_the_cursor() {
        let mut s = Screen::new(TerminalSize::new(2, 8));
        s.print("hi");
        assert_eq!(content_at(&s, 0, 0), "h");
        assert_eq!(content_at(&s, 0, 1), "i");
        assert_eq!(s.cursor().position(), Position::new(0, 2));
    }

    #[test]
    fn print_applies_the_current_pen() {
        let mut s = Screen::new(TerminalSize::new(1, 4));
        s.pen_mut().fg = Color::RED;
        s.print("a");
        let cell = s.cell(Position::new(0, 0)).unwrap();
        assert_eq!(cell.fg, Color::RED);
    }

    #[test]
    fn filling_a_row_defers_the_wrap_until_the_next_glyph() {
        let mut s = Screen::new(TerminalSize::new(2, 3));
        s.print("abc");
        // Row full; cursor sits on the last column, wrap armed, no scroll yet.
        assert_eq!(s.cursor().position(), Position::new(0, 2));
        assert!(s.cursor().pending_wrap());
        assert!(!s.row(0).unwrap().is_wrapped());
        // Next glyph performs the wrap into a continuation of the same line.
        s.print("d");
        assert!(s.row(0).unwrap().is_wrapped());
        assert_eq!(s.row(1).unwrap().line_id(), s.row(0).unwrap().line_id());
        assert_eq!(content_at(&s, 1, 0), "d");
        assert_eq!(s.cursor().position(), Position::new(1, 1));
    }

    #[test]
    fn wide_char_writes_a_spacer_and_advances_by_two() {
        let mut s = Screen::new(TerminalSize::new(1, 4));
        s.print("a\u{4E2D}b");
        assert_eq!(content_at(&s, 0, 0), "a");
        assert_eq!(content_at(&s, 0, 1), "\u{4E2D}");
        assert_eq!(content_at(&s, 0, 2), ""); // spacer
        assert_eq!(content_at(&s, 0, 3), "b");
    }

    #[test]
    fn a_wide_char_that_would_not_fit_wraps_and_leaves_padding() {
        let mut s = Screen::new(TerminalSize::new(2, 2));
        s.print("a\u{4E2D}");
        // 'a' at (0,0); the wide char cannot fit in the one remaining column, so
        // it wraps, leaving (0,1) as padding.
        assert_eq!(content_at(&s, 0, 0), "a");
        assert!(s.cell(Position::new(0, 1)).unwrap().is_blank());
        assert!(s.row(0).unwrap().is_wrapped());
        assert_eq!(content_at(&s, 1, 0), "\u{4E2D}");
    }

    #[test]
    fn carriage_return_moves_to_column_zero() {
        let mut s = Screen::new(TerminalSize::new(2, 8));
        s.print("hello");
        s.carriage_return();
        assert_eq!(s.cursor().position(), Position::new(0, 0));
    }

    #[test]
    fn line_feed_moves_down_then_scrolls_and_freezes_top() {
        let mut s = Screen::new(TerminalSize::new(2, 4));
        s.print("X");
        s.line_feed();
        assert_eq!(s.cursor().position(), Position::new(1, 1));
        s.carriage_return();
        s.print("Y");
        // Now on the last row: a line feed scrolls, freezing "X" into scrollback.
        s.line_feed();
        assert_eq!(s.scrollback().len(), 1);
        assert_eq!(s.scrollback().iter().next().unwrap().text(), "X");
        // "Y" is now the top active row.
        assert_eq!(content_at(&s, 0, 0), "Y");
    }

    #[test]
    fn a_soft_wrapped_line_freezes_as_one_split_head() {
        // One row, width 2: "abcd" wraps repeatedly against the single row.
        let mut s = Screen::new(TerminalSize::new(1, 2));
        s.print("abcd");
        // "ab" scrolled off as the frozen head; "cd" is the live tail. They are
        // one logical line (same id), split across the boundary (#24).
        let head = s.pending_head().expect("straddling head");
        assert_eq!(head.text(), "ab");
        assert_eq!(content_at(&s, 0, 0), "c");
        assert_eq!(content_at(&s, 0, 1), "d");
        assert_eq!(s.row(0).unwrap().line_id(), head.id());
    }

    #[test]
    fn combining_mark_attaches_to_the_previous_cell() {
        let mut s = Screen::new(TerminalSize::new(1, 4));
        s.print("e");
        s.print("\u{301}");
        assert_eq!(content_at(&s, 0, 0), "e\u{301}");
        // It did not consume a new cell.
        assert_eq!(s.cursor().position(), Position::new(0, 1));
    }

    fn row_str(s: &Screen, r: u16) -> String {
        s.row(r)
            .unwrap()
            .cells()
            .iter()
            .map(|c| c.content.as_str())
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    #[test]
    fn resize_to_same_size_is_a_noop() {
        let mut s = Screen::new(TerminalSize::new(3, 6));
        s.print("hello");
        let before = s.cursor().position();
        s.resize(TerminalSize::new(3, 6));
        assert_eq!(s.cursor().position(), before);
        assert_eq!(row_str(&s, 0), "hello");
    }

    #[test]
    fn narrowing_reflows_a_wrapped_line_and_keeps_the_cursor_on_its_text() {
        let mut s = Screen::new(TerminalSize::new(3, 6));
        s.print("abcdefghij"); // wraps: row0 "abcdef", row1 "ghij", cursor (1,4)
        assert_eq!(s.cursor().position(), Position::new(1, 4));

        s.resize(TerminalSize::new(3, 3));
        // "abcdefghij" at width 3 is abc|def|ghi|j (4 rows), plus the trailing
        // blank line, so 5 display rows against a height of 3: the top two rows
        // ("abc","def") overflow into the straddling head, leaving ghi|j|blank.
        assert_eq!(row_str(&s, 0), "ghi");
        assert_eq!(row_str(&s, 1), "j");
        assert_eq!(row_str(&s, 2), "");
        assert_eq!(s.pending_head().unwrap().text(), "abcdef");
        // Cursor still sits just after 'j'.
        assert_eq!(s.cursor().position(), Position::new(1, 1));
    }

    #[test]
    fn reflow_round_trips_back_to_the_original_layout() {
        let mut s = Screen::new(TerminalSize::new(3, 6));
        s.print("abcdefghij");
        let cursor_before = s.cursor().position();

        s.resize(TerminalSize::new(3, 3));
        s.resize(TerminalSize::new(3, 6));

        // Back to the original wrapping and cursor position (idempotent, §16.3).
        assert_eq!(row_str(&s, 0), "abcdef");
        assert_eq!(row_str(&s, 1), "ghij");
        assert_eq!(s.cursor().position(), cursor_before);
    }

    #[test]
    fn growing_width_unwraps_a_line_onto_one_row() {
        let mut s = Screen::new(TerminalSize::new(3, 4));
        s.print("abcdef"); // width 4: row0 "abcd", row1 "ef"
        assert_eq!(row_str(&s, 0), "abcd");
        assert_eq!(row_str(&s, 1), "ef");
        s.resize(TerminalSize::new(3, 8));
        assert_eq!(row_str(&s, 0), "abcdef");
        assert_eq!(row_str(&s, 1), "");
    }

    #[test]
    fn styles_survive_reflow() {
        let mut s = Screen::new(TerminalSize::new(2, 6));
        s.pen_mut().fg = Color::RED;
        s.print("abcd");
        s.resize(TerminalSize::new(2, 2));
        // 'd' stays in the active grid after the reflow, still red.
        let d = s
            .rows()
            .flat_map(|r| r.cells())
            .find(|c| c.content == "d")
            .unwrap();
        assert_eq!(d.fg, Color::RED);
    }
}
