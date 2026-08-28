//! The primary screen: the mutable active grid plus its scrollback.
//!
//! PRD §17.5 #21: a dedicated type for the primary buffer, distinct from the
//! dumb [`Grid`](crate::prelude::Grid) that backs the alternate screen. The active
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
use crate::parsers::vt::{Command, EraseMode, Sgr, VtParser};
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
    /// The scrolling region (DECSTBM), as inclusive display rows. Line feeds at
    /// `scroll_bottom` scroll the rows between the margins instead of the whole
    /// screen, which is how full-screen programs keep a status line still.
    /// Defaults to the whole screen.
    scroll_top: u16,
    scroll_bottom: u16,
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
            scroll_top: 0,
            scroll_bottom: size.rows.saturating_sub(1),
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
        if row == self.scroll_bottom {
            // At the bottom margin the text moves, not the cursor.
            self.scroll_region_up_once(None);
            self.cursor.move_to(Position::new(row, col));
        } else if (row as usize + 1) < self.rows.len() {
            self.cursor.move_to(Position::new(row + 1, col));
        }
        // Below the region on the last row there is nowhere to go: the cursor
        // stays and nothing scrolls.
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
        if row == self.scroll_bottom {
            // The continuation row is the one the scroll opens up, so it must
            // carry the wrapping line's id rather than a fresh one.
            self.scroll_region_up_once(Some(line_id));
            self.cursor.move_to(Position::new(row, 0));
        } else if (row as usize + 1) < self.rows.len() {
            if let Some(next) = self.rows.get_mut(row as usize + 1) {
                next.set_line_id(line_id);
                next.set_wrapped(false);
            }
            self.cursor.move_to(Position::new(row + 1, 0));
        }
    }

    /// Whether the scrolling region covers the whole screen, which is the only
    /// case in which scrolled-off rows join scrollback.
    ///
    /// Scrollback is the history of the session, not of a pane: when a program
    /// has carved out a region (a pager's text area above a status line), rows
    /// pushed out of that region are that program's business and are dropped.
    fn region_is_whole_screen(&self) -> bool {
        self.scroll_top == 0 && self.scroll_bottom as usize + 1 == self.rows.len()
    }

    /// Insert a blank row at `at`, giving it `line_id` (or a fresh one).
    ///
    /// If the row above belongs to a different logical line, its soft-wrap flag
    /// is cleared: it can no longer be continued by the row below it, and a
    /// stale flag would mislead reflow.
    fn insert_blank_row(&mut self, at: usize, line_id: Option<LineId>) {
        let id = match line_id {
            Some(id) => id,
            None => self.alloc_line_id(),
        };
        self.rows.insert(at, Row::blank(self.size.cols, id));
        if at > 0
            && let Some(prev) = self.rows.get_mut(at - 1)
            && prev.line_id() != id
        {
            prev.set_wrapped(false);
        }
    }

    /// Scroll the region up by one row: the top row of the region leaves (into
    /// scrollback only if the region is the whole screen) and a blank row opens
    /// at the bottom, carrying `line_id` when it continues a wrapped line.
    fn scroll_region_up_once(&mut self, line_id: Option<LineId>) {
        let (top, bottom) = (self.scroll_top as usize, self.scroll_bottom as usize);
        if top > bottom || bottom >= self.rows.len() {
            return;
        }
        let whole = self.region_is_whole_screen();
        if let Some(row) = self.rows.remove(top)
            && whole
        {
            self.freeze_row(row);
        }
        self.insert_blank_row(bottom, line_id);
    }

    /// Scroll the region down by one row: the bottom row of the region is
    /// discarded and a blank row opens at the top. Nothing reaches scrollback —
    /// scrollback is above the screen, and this pushes content the other way.
    fn scroll_region_down_once(&mut self) {
        let (top, bottom) = (self.scroll_top as usize, self.scroll_bottom as usize);
        if top > bottom || bottom >= self.rows.len() {
            return;
        }
        self.rows.remove(bottom);
        self.insert_blank_row(top, None);
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
        self.reset_scroll_region();

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
        self.reset_scroll_region();
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

/// The VT command applier and the cursor/erase editing operations it drives.
/// These are the bounds-clamped grid edits a parsed [`Command`] maps onto; scroll
/// regions are not modelled yet, so moves clamp to the whole screen.
impl Screen {
    /// Feed bytes through `parser` and apply the resulting commands. This is the
    /// engine's main input entry point (the PTY reader calls it).
    pub fn advance(&mut self, parser: &mut VtParser, bytes: &[u8]) {
        for cmd in parser.feed(bytes) {
            self.apply(&cmd);
        }
    }

    /// Apply one parsed VT command.
    pub fn apply(&mut self, cmd: &Command) {
        match cmd {
            Command::Print(s) => self.print(s),
            Command::Bell => {} // no audible/visual bell yet
            Command::Backspace => self.backspace(),
            Command::Tab => self.tab(),
            Command::LineFeed => self.line_feed(),
            Command::CarriageReturn => self.carriage_return(),
            Command::CursorUp(n) => self.cursor_up(*n),
            Command::CursorDown(n) => self.cursor_down(*n),
            Command::CursorForward(n) => self.cursor_right(*n),
            Command::CursorBack(n) => self.cursor_left(*n),
            Command::CursorPosition { row, col } => self.cursor_to(*row, *col),
            Command::CursorColumn(col) => {
                let row = self.cursor.row();
                self.cursor_to(row, *col);
            }
            Command::CursorLine(row) => {
                let col = self.cursor.col();
                self.cursor_to(*row, col);
            }
            Command::Index => self.index(),
            Command::ReverseIndex => self.reverse_index(),
            Command::NextLine => self.next_line(),
            Command::SetScrollRegion { top, bottom } => self.set_scroll_region(*top, *bottom),
            Command::ScrollUp(n) => self.scroll_region_up(*n),
            Command::ScrollDown(n) => self.scroll_region_down(*n),
            Command::InsertLines(n) => self.insert_lines(*n),
            Command::DeleteLines(n) => self.delete_lines(*n),
            Command::InsertChars(n) => self.insert_chars(*n),
            Command::DeleteChars(n) => self.delete_chars(*n),
            Command::EraseChars(n) => self.erase_chars(*n),
            Command::EraseInDisplay(mode) => self.erase_in_display(*mode),
            Command::EraseInLine(mode) => self.erase_in_line(*mode),
            Command::Sgr(list) => {
                for sgr in list {
                    self.apply_sgr(*sgr);
                }
            }
            Command::Ignored => {}
        }
    }

    fn apply_sgr(&mut self, sgr: Sgr) {
        match sgr {
            Sgr::Reset => self.pen = Pen::default(),
            Sgr::Bold => self.pen.attrs.insert(CellAttrs::BOLD),
            Sgr::Dim => self.pen.attrs.insert(CellAttrs::DIM),
            Sgr::Italic => self.pen.attrs.insert(CellAttrs::ITALIC),
            Sgr::Underline => self.pen.attrs.insert(CellAttrs::UNDERLINE),
            Sgr::Reverse => self.pen.attrs.insert(CellAttrs::REVERSE),
            Sgr::Hidden => self.pen.attrs.insert(CellAttrs::HIDDEN),
            Sgr::Strikethrough => self.pen.attrs.insert(CellAttrs::STRIKETHROUGH),
            Sgr::NoBoldDim => self.pen.attrs.remove(CellAttrs::BOLD | CellAttrs::DIM),
            Sgr::NoItalic => self.pen.attrs.remove(CellAttrs::ITALIC),
            Sgr::NoUnderline => self.pen.attrs.remove(CellAttrs::UNDERLINE),
            Sgr::NoReverse => self.pen.attrs.remove(CellAttrs::REVERSE),
            Sgr::NoHidden => self.pen.attrs.remove(CellAttrs::HIDDEN),
            Sgr::NoStrikethrough => self.pen.attrs.remove(CellAttrs::STRIKETHROUGH),
            Sgr::Fg(c) => self.pen.fg = c,
            Sgr::Bg(c) => self.pen.bg = c,
            Sgr::DefaultFg => self.pen.fg = Color::Default,
            Sgr::DefaultBg => self.pen.bg = Color::Default,
        }
    }

    fn clamp(&self, row: u16, col: u16) -> Position {
        Position::new(
            row.min(self.size.rows.saturating_sub(1)),
            col.min(self.size.cols.saturating_sub(1)),
        )
    }

    /// Absolute cursor move, clamped to the screen.
    pub fn cursor_to(&mut self, row: u16, col: u16) {
        let pos = self.clamp(row, col);
        self.cursor.move_to(pos);
    }

    pub fn cursor_up(&mut self, n: u16) {
        let pos = self.clamp(self.cursor.row().saturating_sub(n), self.cursor.col());
        self.cursor.move_to(pos);
    }

    pub fn cursor_down(&mut self, n: u16) {
        let pos = self.clamp(self.cursor.row().saturating_add(n), self.cursor.col());
        self.cursor.move_to(pos);
    }

    pub fn cursor_left(&mut self, n: u16) {
        let pos = self.clamp(self.cursor.row(), self.cursor.col().saturating_sub(n));
        self.cursor.move_to(pos);
    }

    pub fn cursor_right(&mut self, n: u16) {
        let pos = self.clamp(self.cursor.row(), self.cursor.col().saturating_add(n));
        self.cursor.move_to(pos);
    }

    /// Backspace: move the cursor one column left (it does not erase).
    pub fn backspace(&mut self) {
        self.cursor_left(1);
    }

    /// Advance to the next tab stop (every 8 columns), clamped to the last column.
    pub fn tab(&mut self) {
        let next = (self.cursor.col() / 8).saturating_add(1).saturating_mul(8);
        let pos = self.clamp(self.cursor.row(), next);
        self.cursor.move_to(pos);
    }

    /// A cell an erase writes: a blank carrying the current background colour, so
    /// a coloured erase fills with that background (ECMA-48).
    fn erase_cell(&self) -> Cell {
        Cell {
            content: CompactString::const_new(" "),
            fg: Color::Default,
            bg: self.pen.bg,
            attrs: CellAttrs::EMPTY,
        }
    }

    fn fill_row(&mut self, row: usize, start: usize, end: usize, cell: &Cell) {
        if let Some(r) = self.rows.get_mut(row) {
            let cells = r.cells_mut();
            let end = end.min(cells.len());
            let start = start.min(end);
            for c in &mut cells[start..end] {
                *c = cell.clone();
            }
        }
    }

    /// The scrolling region as inclusive display rows (DECSTBM).
    pub fn scroll_region(&self) -> (u16, u16) {
        (self.scroll_top, self.scroll_bottom)
    }

    /// Set the scrolling region. `bottom` of `None` means the last row.
    ///
    /// A region that is empty or reversed is ignored outright (DECSTBM), and a
    /// valid one homes the cursor. Both are the VT100 behaviour, and programs
    /// depend on the homing: `CSI r` then `CSI H` is a very common reset.
    pub fn set_scroll_region(&mut self, top: u16, bottom: Option<u16>) {
        let last = self.size.rows.saturating_sub(1);
        let bottom = bottom.unwrap_or(last).min(last);
        if top >= bottom {
            return;
        }
        self.scroll_top = top;
        self.scroll_bottom = bottom;
        self.cursor.move_to(Position::new(0, 0));
    }

    fn reset_scroll_region(&mut self) {
        self.scroll_top = 0;
        self.scroll_bottom = self.size.rows.saturating_sub(1);
    }

    /// IND: down one row, scrolling the region at the bottom margin. Identical
    /// to a line feed; it exists because the two arrive as different bytes.
    pub fn index(&mut self) {
        self.line_feed();
    }

    /// RI: up one row, scrolling the region down at the top margin.
    pub fn reverse_index(&mut self) {
        let row = self.cursor.row();
        let col = self.cursor.col();
        if row == self.scroll_top {
            self.scroll_region_down_once();
            self.cursor.move_to(Position::new(row, col));
        } else if row > 0 {
            self.cursor.move_to(Position::new(row - 1, col));
        }
    }

    /// NEL: a line feed plus a carriage return.
    pub fn next_line(&mut self) {
        self.line_feed();
        self.carriage_return();
    }

    /// SU: scroll the region up `n` rows, leaving the cursor where it is.
    pub fn scroll_region_up(&mut self, n: u16) {
        for _ in 0..n {
            self.scroll_region_up_once(None);
        }
    }

    /// SD: scroll the region down `n` rows, leaving the cursor where it is.
    pub fn scroll_region_down(&mut self, n: u16) {
        for _ in 0..n {
            self.scroll_region_down_once();
        }
    }

    /// Whether the cursor sits inside the scrolling region. Line-editing
    /// commands do nothing when it does not (VT100).
    fn cursor_in_region(&self) -> bool {
        let row = self.cursor.row();
        row >= self.scroll_top && row <= self.scroll_bottom
    }

    /// IL: open `n` blank rows at the cursor, pushing the rest of the region
    /// down and off the bottom margin. The cursor moves to column 0.
    pub fn insert_lines(&mut self, n: u16) {
        if !self.cursor_in_region() {
            return;
        }
        let at = self.cursor.row() as usize;
        let bottom = self.scroll_bottom as usize;
        for _ in 0..n.min((bottom - at + 1) as u16) {
            self.rows.remove(bottom);
            self.insert_blank_row(at, None);
        }
        self.carriage_return();
    }

    /// DL: remove `n` rows at the cursor, pulling the rest of the region up and
    /// opening blanks at the bottom margin. The cursor moves to column 0.
    pub fn delete_lines(&mut self, n: u16) {
        if !self.cursor_in_region() {
            return;
        }
        let at = self.cursor.row() as usize;
        let bottom = self.scroll_bottom as usize;
        for _ in 0..n.min((bottom - at + 1) as u16) {
            self.rows.remove(at);
            self.insert_blank_row(bottom, None);
        }
        self.carriage_return();
    }

    /// ICH: open `n` blank cells at the cursor, shifting the rest of the row
    /// right; cells pushed past the last column are lost.
    pub fn insert_chars(&mut self, n: u16) {
        let cell = self.erase_cell();
        let col = self.cursor.col() as usize;
        let Some(row) = self.rows.get_mut(self.cursor.row() as usize) else {
            return;
        };
        let cells = row.cells_mut();
        let n = (n as usize).min(cells.len().saturating_sub(col));
        for i in (col + n..cells.len()).rev() {
            cells[i] = cells[i - n].clone();
        }
        for c in &mut cells[col..col + n] {
            *c = cell.clone();
        }
    }

    /// DCH: remove `n` cells at the cursor, shifting the rest of the row left
    /// and blanking the end.
    pub fn delete_chars(&mut self, n: u16) {
        let cell = self.erase_cell();
        let col = self.cursor.col() as usize;
        let Some(row) = self.rows.get_mut(self.cursor.row() as usize) else {
            return;
        };
        let cells = row.cells_mut();
        let n = (n as usize).min(cells.len().saturating_sub(col));
        let len = cells.len();
        for i in col..len - n {
            cells[i] = cells[i + n].clone();
        }
        for c in &mut cells[len - n..] {
            *c = cell.clone();
        }
    }

    /// ECH: overwrite `n` cells from the cursor with blanks. Nothing shifts, so
    /// this is an erase, not a delete.
    pub fn erase_chars(&mut self, n: u16) {
        let cell = self.erase_cell();
        let row = self.cursor.row() as usize;
        let col = self.cursor.col() as usize;
        self.fill_row(row, col, col + n as usize, &cell);
    }

    /// Erase within the cursor's row (§ CSI K).
    pub fn erase_in_line(&mut self, mode: EraseMode) {
        let cell = self.erase_cell();
        let row = self.cursor.row() as usize;
        let col = self.cursor.col() as usize;
        let cols = self.size.cols as usize;
        match mode {
            EraseMode::ToEnd => self.fill_row(row, col, cols, &cell),
            EraseMode::ToStart => self.fill_row(row, 0, col + 1, &cell),
            EraseMode::All => self.fill_row(row, 0, cols, &cell),
        }
    }

    /// Erase within the display (§ CSI J). Does not touch scrollback.
    pub fn erase_in_display(&mut self, mode: EraseMode) {
        let cell = self.erase_cell();
        let row = self.cursor.row() as usize;
        let col = self.cursor.col() as usize;
        let cols = self.size.cols as usize;
        let nrows = self.rows.len();
        match mode {
            EraseMode::ToEnd => {
                self.fill_row(row, col, cols, &cell);
                for r in (row + 1)..nrows {
                    self.fill_row(r, 0, cols, &cell);
                }
            }
            EraseMode::ToStart => {
                for r in 0..row {
                    self.fill_row(r, 0, cols, &cell);
                }
                self.fill_row(row, 0, col + 1, &cell);
            }
            EraseMode::All => {
                for r in 0..nrows {
                    self.fill_row(r, 0, cols, &cell);
                }
            }
        }
    }
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

    #[test]
    fn cursor_moves_are_clamped_to_the_screen() {
        let mut s = Screen::new(TerminalSize::new(3, 4));
        s.cursor_to(10, 10);
        assert_eq!(s.cursor().position(), Position::new(2, 3));
        s.cursor_up(1);
        assert_eq!(s.cursor().position(), Position::new(1, 3));
        s.cursor_left(2);
        assert_eq!(s.cursor().position(), Position::new(1, 1));
        s.cursor_down(9);
        assert_eq!(s.cursor().position(), Position::new(2, 1));
    }

    #[test]
    fn backspace_and_tab_move_the_cursor() {
        let mut s = Screen::new(TerminalSize::new(1, 20));
        s.cursor_to(0, 3);
        s.backspace();
        assert_eq!(s.cursor().col(), 2);
        s.tab();
        assert_eq!(s.cursor().col(), 8); // next 8-col stop
        s.tab();
        assert_eq!(s.cursor().col(), 16);
    }

    #[test]
    fn erase_in_line_clears_the_requested_span() {
        let mut s = Screen::new(TerminalSize::new(1, 6));
        s.print("abcdef");
        s.cursor_to(0, 3);
        s.erase_in_line(EraseMode::ToEnd);
        assert_eq!(row_str(&s, 0), "abc"); // cols 3.. blanked

        let mut s = Screen::new(TerminalSize::new(1, 6));
        s.print("abcdef");
        s.cursor_to(0, 2);
        s.erase_in_line(EraseMode::ToStart);
        // cols 0..=2 blanked, "def" remains from col 3.
        assert_eq!(row_str(&s, 0), "   def");
    }

    #[test]
    fn erase_in_display_to_end_clears_below() {
        let mut s = Screen::new(TerminalSize::new(2, 4));
        s.print("ab");
        s.line_feed();
        s.carriage_return();
        s.print("cd");
        s.cursor_to(0, 1);
        s.erase_in_display(EraseMode::ToEnd);
        assert_eq!(row_str(&s, 0), "a"); // from col 1 on row 0
        assert_eq!(row_str(&s, 1), ""); // whole row below cleared
    }

    #[test]
    fn colored_erase_fills_with_the_current_background() {
        let mut s = Screen::new(TerminalSize::new(1, 4));
        s.pen_mut().bg = Color::RED;
        s.erase_in_line(EraseMode::All);
        assert!(s.rows().flat_map(|r| r.cells()).all(|c| c.bg == Color::RED));
    }

    #[test]
    fn advance_drives_the_screen_from_bytes() {
        use crate::parsers::vt::VtParser;
        let mut s = Screen::new(TerminalSize::new(3, 10));
        let mut p = VtParser::new();
        // Print, newline, then a red bold 'X'.
        s.advance(&mut p, b"hi\r\n\x1b[1;31mX");
        assert_eq!(row_str(&s, 0), "hi");
        let x = s.cell(Position::new(1, 0)).unwrap();
        assert_eq!(x.content, "X");
        assert_eq!(x.fg, Color::RED);
        assert!(x.attrs.contains(CellAttrs::BOLD));
    }

    /// A screen whose rows read "r0".."rN", one word per row, cursor left home.
    fn numbered(rows: u16, cols: u16) -> Screen {
        let mut s = Screen::new(TerminalSize::new(rows, cols));
        for r in 0..rows {
            s.cursor_to(r, 0);
            s.print(&format!("r{r}"));
        }
        s.cursor_to(0, 0);
        s
    }

    fn rows_str(s: &Screen) -> Vec<String> {
        (0..s.size().rows).map(|r| row_str(s, r)).collect()
    }

    #[test]
    fn a_new_screen_scrolls_over_its_whole_height() {
        let s = Screen::new(TerminalSize::new(4, 8));
        assert_eq!(s.scroll_region(), (0, 3));
    }

    #[test]
    fn setting_a_region_homes_the_cursor() {
        let mut s = Screen::new(TerminalSize::new(6, 8));
        s.cursor_to(4, 4);
        s.set_scroll_region(1, Some(3));
        assert_eq!(s.scroll_region(), (1, 3));
        assert_eq!(s.cursor().position(), Position::new(0, 0));
    }

    #[test]
    fn an_empty_or_reversed_region_is_ignored() {
        let mut s = Screen::new(TerminalSize::new(6, 8));
        s.set_scroll_region(3, Some(1));
        assert_eq!(s.scroll_region(), (0, 5), "reversed: ignored");
        s.set_scroll_region(2, Some(2));
        assert_eq!(s.scroll_region(), (0, 5), "single row: ignored");
        s.set_scroll_region(2, Some(99));
        assert_eq!(s.scroll_region(), (2, 5), "bottom clamped to the last row");
    }

    #[test]
    fn a_line_feed_at_the_bottom_margin_scrolls_only_the_region() {
        let mut s = numbered(5, 8);
        s.set_scroll_region(1, Some(3));
        s.cursor_to(3, 0);
        s.line_feed();
        assert_eq!(rows_str(&s), ["r0", "r2", "r3", "", "r4"]);
        assert_eq!(s.cursor().position(), Position::new(3, 0), "cursor stays");
    }

    #[test]
    fn rows_scrolled_out_of_a_partial_region_do_not_reach_scrollback() {
        // Scrollback is the session's history, not a pane's: a program that
        // carved out a region owns what falls out of it.
        let mut s = numbered(5, 8);
        s.set_scroll_region(1, Some(3));
        s.cursor_to(3, 0);
        s.line_feed();
        assert!(s.scrollback().is_empty());
        assert!(s.pending_head().is_none());
    }

    #[test]
    fn a_whole_screen_region_still_feeds_scrollback() {
        let mut s = numbered(3, 8);
        s.cursor_to(2, 0);
        s.line_feed();
        assert_eq!(rows_str(&s), ["r1", "r2", ""]);
        assert_eq!(s.scrollback().len(), 1);
    }

    #[test]
    fn a_line_feed_below_the_region_neither_moves_nor_scrolls() {
        let mut s = numbered(4, 8);
        s.set_scroll_region(0, Some(2));
        s.cursor_to(3, 0);
        s.line_feed();
        assert_eq!(rows_str(&s), ["r0", "r1", "r2", "r3"]);
        assert_eq!(s.cursor().position(), Position::new(3, 0));
    }

    #[test]
    fn reverse_index_at_the_top_margin_scrolls_the_region_down() {
        let mut s = numbered(5, 8);
        s.set_scroll_region(1, Some(3));
        s.cursor_to(1, 0);
        s.reverse_index();
        assert_eq!(rows_str(&s), ["r0", "", "r1", "r2", "r4"]);
        assert_eq!(s.cursor().position(), Position::new(1, 0));
    }

    #[test]
    fn reverse_index_elsewhere_just_moves_up() {
        let mut s = numbered(4, 8);
        s.cursor_to(2, 3);
        s.reverse_index();
        assert_eq!(s.cursor().position(), Position::new(1, 3));
        assert_eq!(rows_str(&s), ["r0", "r1", "r2", "r3"]);
    }

    #[test]
    fn next_line_feeds_and_returns() {
        let mut s = numbered(3, 8);
        s.cursor_to(0, 5);
        s.next_line();
        assert_eq!(s.cursor().position(), Position::new(1, 0));
    }

    #[test]
    fn autowrap_at_the_bottom_margin_wraps_inside_the_region() {
        let mut s = Screen::new(TerminalSize::new(4, 3));
        s.set_scroll_region(1, Some(2));
        s.cursor_to(2, 0);
        s.print("abcde");
        // The wrap scrolls the region, so the tail lands on the bottom margin
        // and the row above holds the head.
        assert_eq!(row_str(&s, 1), "abc");
        assert_eq!(row_str(&s, 2), "de");
        assert_eq!(
            s.row(1).unwrap().line_id(),
            s.row(2).unwrap().line_id(),
            "both rows belong to the one wrapped logical line"
        );
        assert!(s.row(1).unwrap().is_wrapped());
    }

    #[test]
    fn scroll_up_and_down_move_text_without_the_cursor() {
        let mut s = numbered(5, 8);
        s.set_scroll_region(1, Some(3));
        s.cursor_to(2, 4);
        s.scroll_region_up(2);
        assert_eq!(rows_str(&s), ["r0", "r3", "", "", "r4"]);
        assert_eq!(s.cursor().position(), Position::new(2, 4));
        s.scroll_region_down(1);
        assert_eq!(rows_str(&s), ["r0", "", "r3", "", "r4"]);
    }

    #[test]
    fn insert_lines_pushes_the_region_down_and_off_the_margin() {
        let mut s = numbered(5, 8);
        s.set_scroll_region(1, Some(3));
        s.cursor_to(2, 4);
        s.insert_lines(1);
        assert_eq!(rows_str(&s), ["r0", "r1", "", "r2", "r4"]);
        assert_eq!(s.cursor().position(), Position::new(2, 0));
    }

    #[test]
    fn delete_lines_pulls_the_region_up_and_blanks_the_margin() {
        let mut s = numbered(5, 8);
        s.set_scroll_region(1, Some(3));
        s.cursor_to(1, 0);
        s.delete_lines(1);
        assert_eq!(rows_str(&s), ["r0", "r2", "r3", "", "r4"]);
        assert!(s.scrollback().is_empty(), "deleted lines are not history");
    }

    #[test]
    fn line_editing_outside_the_region_does_nothing() {
        let mut s = numbered(5, 8);
        s.set_scroll_region(1, Some(3));
        s.cursor_to(4, 0);
        s.insert_lines(2);
        s.delete_lines(2);
        assert_eq!(rows_str(&s), ["r0", "r1", "r2", "r3", "r4"]);
    }

    #[test]
    fn more_lines_than_the_region_holds_clears_it() {
        let mut s = numbered(5, 8);
        s.set_scroll_region(1, Some(3));
        s.cursor_to(1, 0);
        s.delete_lines(99);
        assert_eq!(rows_str(&s), ["r0", "", "", "", "r4"]);
    }

    #[test]
    fn insert_and_delete_chars_shift_within_the_row() {
        let mut s = Screen::new(TerminalSize::new(1, 6));
        s.print("abcdef");
        s.cursor_to(0, 1);
        s.insert_chars(2);
        assert_eq!(row_str(&s, 0), "a  bcd", "the tail falls off the end");
        s.cursor_to(0, 1);
        s.delete_chars(2);
        assert_eq!(row_str(&s, 0), "abcd");
    }

    #[test]
    fn erase_chars_blanks_in_place_without_shifting() {
        let mut s = Screen::new(TerminalSize::new(1, 6));
        s.print("abcdef");
        s.cursor_to(0, 2);
        s.erase_chars(2);
        assert_eq!(row_str(&s, 0), "ab  ef");
    }

    #[test]
    fn character_editing_clamps_to_the_row() {
        let mut s = Screen::new(TerminalSize::new(1, 4));
        s.print("abcd");
        s.cursor_to(0, 2);
        s.insert_chars(99);
        assert_eq!(row_str(&s, 0), "ab");
        s.cursor_to(0, 0);
        s.delete_chars(99);
        assert_eq!(row_str(&s, 0), "");
        s.erase_chars(99);
        assert_eq!(row_str(&s, 0), "");
    }

    #[test]
    fn character_editing_uses_the_erase_background() {
        let mut s = Screen::new(TerminalSize::new(1, 4));
        s.print("ab");
        s.pen_mut().bg = Color::BLUE;
        s.cursor_to(0, 0);
        s.insert_chars(1);
        assert_eq!(s.cell(Position::new(0, 0)).unwrap().bg, Color::BLUE);
    }

    #[test]
    fn a_resize_resets_the_scrolling_region() {
        // The margins are display coordinates; after a reflow they no longer
        // mean what the program that set them intended.
        let mut s = numbered(5, 8);
        s.set_scroll_region(1, Some(3));
        s.resize(TerminalSize::new(4, 8));
        assert_eq!(s.scroll_region(), (0, 3));
    }

    #[test]
    fn a_region_scroll_drives_from_bytes() {
        let mut s = numbered(5, 8);
        let mut p = VtParser::new();
        // Region rows 2..4 (1-based), cursor to the bottom margin, line feed.
        s.advance(&mut p, b"\x1b[2;4r\x1b[4;1H\n");
        assert_eq!(rows_str(&s), ["r0", "r2", "r3", "", "r4"]);
    }

    #[test]
    fn sgr_reset_clears_the_pen() {
        let mut s = Screen::new(TerminalSize::new(1, 4));
        let mut p = VtParser::new();
        s.advance(&mut p, b"\x1b[1;31m");
        assert_eq!(s.pen().fg, Color::RED);
        s.advance(&mut p, b"\x1b[0m");
        assert_eq!(s.pen(), Pen::default());
    }
}
