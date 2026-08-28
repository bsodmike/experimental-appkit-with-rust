//! The read path: turning the visible screen into a flat frame of runs.
//!
//! PRD §10 settles the shape of what the frontend reads. It does *not* get a
//! pointer into the grid (option A there: the caller owns the buffer, the
//! engine copies into it under the lock), and what it gets is *runs*, not
//! cells: a run is a span of consecutive columns sharing one style, described
//! as a slice of an accompanying UTF-8 buffer. Runs are what Core Text draws,
//! they are what keeps combining marks shaped with their base character, and
//! there are far fewer of them per frame than there are cells.
//!
//! This module builds that frame purely, in Rust, with no FFI in sight: a
//! [`Frame`] owns the text buffer and the [`Run`]s pointing into it, and
//! [`Screen::render_into`] refills an existing frame so the steady state
//! allocates nothing. The `terminal-ffi` crate later copies these same two
//! buffers across the boundary; keeping the shape here means it can be tested
//! headlessly (PRD §18).
//!
//! ## Coalescing rules
//!
//! - Trailing blank cells are dropped: a row of 200 columns holding "ls" is two
//!   glyphs, not 198 spaces the frontend must paint.
//! - A blank cell (a space with a default background and no attributes) joins
//!   the current run whenever the run itself would render nothing on it, so
//!   `hello world` stays one run rather than three.
//! - A run that is still all-blank adopts the style of the first cell that
//!   would actually draw, provided that style is invisible on the preceding
//!   spaces — which keeps leading indentation attached to the text it precedes.
//! - The trailing spacer column of a wide grapheme contributes a column and no
//!   bytes, so `cols` counts display columns while `utf8_len` counts text.

use crate::cell::Cell;
use crate::geometry::{Position, TerminalSize};
use crate::screen::Screen;

/// One span of consecutive columns sharing a style, as it crosses the FFI
/// boundary (PRD §10).
///
/// `utf8_offset`/`utf8_len` slice the frame's text buffer; `col`/`cols` say
/// where to draw and how many display columns the span covers (a wide grapheme
/// counts 2). `fg`/`bg` are packed by [`Color::pack`](crate::prelude::Color::pack)
/// so the terminal default stays distinguishable from any concrete colour, and
/// `attrs` is the raw [`CellAttrs`](crate::prelude::CellAttrs) bit-set.
///
/// `row` is not in the PRD's original sketch: it is here so one flat array of
/// runs is self-describing and the copy-out API stays a single buffer with a
/// single length, rather than runs plus a parallel row index (see
/// `docs/adrs/2026-08-28.adr-render-frame.md`). The field order is chosen so
/// the struct packs to 24 bytes with no padding.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Run {
    /// Byte offset of this run's text within the frame's buffer.
    pub utf8_offset: u32,
    /// Length in bytes of this run's text.
    pub utf8_len: u32,
    pub fg: u32,
    pub bg: u32,
    /// Display row, zero-based from the top of the viewport.
    pub row: u16,
    /// Starting display column.
    pub col: u16,
    /// Display columns covered; wide graphemes count 2.
    pub cols: u16,
    /// Raw `CellAttrs` bits.
    pub attrs: u16,
}

/// One frame's worth of visible screen: every run, plus the text they slice.
///
/// A frame is a snapshot. It is built under whatever lock guards the screen and
/// read afterwards at leisure, which is what makes it internally consistent —
/// no tearing between row 3 and row 40 (PRD §10).
///
/// Reuse one frame across redraws with [`Screen::render_into`]: it clears the
/// buffers but keeps their capacity, so a steady-state redraw allocates nothing.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Frame {
    size: TerminalSize,
    cursor: Position,
    cursor_visible: bool,
    text: String,
    runs: Vec<Run>,
}

impl Default for Frame {
    fn default() -> Self {
        Self::new()
    }
}

impl Frame {
    /// An empty frame, ready to be filled by [`Screen::render_into`]. Its size
    /// is zero until it is rendered into; `TerminalSize` has no default of its
    /// own because a real screen size is never guessed.
    pub fn new() -> Self {
        Self {
            size: TerminalSize::new(0, 0),
            cursor: Position::new(0, 0),
            cursor_visible: true,
            text: String::new(),
            runs: Vec::new(),
        }
    }

    /// The screen size this frame was rendered at.
    pub fn size(&self) -> TerminalSize {
        self.size
    }

    /// The cursor position, captured in the same snapshot as the runs so the
    /// caret never disagrees with the text under it.
    pub fn cursor(&self) -> Position {
        self.cursor
    }

    /// Whether the frontend should draw a caret (DECTCEM). A full-screen
    /// program that hides the cursor while redrawing expects this to be obeyed.
    pub fn cursor_visible(&self) -> bool {
        self.cursor_visible
    }

    /// The shared UTF-8 buffer the runs slice.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Every run in the frame, ordered by row and then by column.
    pub fn runs(&self) -> &[Run] {
        &self.runs
    }

    /// The runs of one display row. Empty for a blank row or a row out of range.
    pub fn row_runs(&self, row: u16) -> &[Run] {
        // Runs are sorted by row, so the row's slice is one contiguous window.
        let start = self.runs.partition_point(|r| r.row < row);
        let end = self.runs[start..].partition_point(|r| r.row == row) + start;
        &self.runs[start..end]
    }

    /// The text of one run, as a slice of the frame's buffer.
    pub fn run_text(&self, run: &Run) -> &str {
        let start = run.utf8_offset as usize;
        &self.text[start..start + run.utf8_len as usize]
    }

    /// The visible text of one display row, with trailing blanks dropped.
    ///
    /// Gaps between runs (a stretch of blank columns that no run covers) are
    /// filled with spaces, so the result lines up with the columns on screen.
    pub fn row_text(&self, row: u16) -> String {
        let mut out = String::new();
        let mut col = 0u16;
        for run in self.row_runs(row) {
            for _ in col..run.col {
                out.push(' ');
            }
            out.push_str(self.run_text(run));
            col = run.col + run.cols;
        }
        out
    }

    fn clear(&mut self) {
        self.text.clear();
        self.runs.clear();
    }
}

impl Screen {
    /// Snapshot the visible screen as a [`Frame`].
    ///
    /// Allocates a fresh frame; prefer [`Screen::render_into`] on the redraw
    /// path, where the frontend reuses one buffer every frame (PRD §10-A).
    pub fn render(&self) -> Frame {
        let mut frame = Frame::new();
        self.render_into(&mut frame);
        frame
    }

    /// Refill `frame` from the visible screen, keeping its existing capacity.
    pub fn render_into(&self, frame: &mut Frame) {
        frame.clear();
        frame.size = self.size();
        frame.cursor = self.cursor().position();
        frame.cursor_visible = self.modes().cursor_visible;
        if self.size().is_empty() {
            return;
        }
        for (row, screen_row) in self.rows().enumerate() {
            build_row(
                row as u16,
                screen_row.cells(),
                &mut frame.text,
                &mut frame.runs,
            );
        }
    }
}

/// Append the runs of one row of cells, pushing their text into `text`.
fn build_row(row: u16, cells: &[Cell], text: &mut String, runs: &mut Vec<Run>) {
    // Trailing blanks are never drawn, so the row ends at its last visible cell.
    let Some(last) = cells.iter().rposition(|c| !c.is_blank()) else {
        return;
    };

    let mut current: Option<Run> = None;
    // Whether every cell in `current` so far renders as nothing, which is what
    // lets a later cell restyle the run instead of starting a new one.
    let mut all_blank = true;

    let mut col = 0usize;
    while col <= last {
        let cell = &cells[col];

        // A double-width cluster is a run of its own, never merged with its
        // neighbours. The frontend positions glyphs by column, and it can only
        // map a glyph back to a column by counting clusters — which works only
        // while every cluster in a run is one column wide. Splitting here is
        // what lets the renderer stay dumb about character widths (PRD §10).
        if is_wide_base(cells, col) {
            if let Some(run) = current.take() {
                runs.push(run);
            }
            let mut wide = Run {
                utf8_offset: text.len() as u32,
                utf8_len: cell.content.len() as u32,
                fg: cell.fg.pack(),
                bg: cell.bg.pack(),
                row,
                col: col as u16,
                cols: 2,
                attrs: cell.attrs.bits(),
            };
            text.push_str(&cell.content);
            // The spacer contributes its column and no bytes.
            wide.utf8_len = cell.content.len() as u32;
            runs.push(wide);
            all_blank = true;
            col += 2;
            continue;
        }

        let blank = cell.is_blank();
        match current.as_mut() {
            // A blank cell extends the run when the run's own style would draw
            // nothing on it: no background to paint, no underline to stroke.
            Some(run) if blank && run.bg == 0 && run.attrs == 0 => {}
            Some(run)
                if !blank
                    && run.fg == cell.fg.pack()
                    && run.bg == cell.bg.pack()
                    && run.attrs == cell.attrs.bits() => {}
            // An all-blank run adopts a style that is invisible on spaces, so
            // leading indentation stays attached to the text it precedes.
            Some(run) if !blank && all_blank && cell.bg.is_default() && cell.attrs.is_empty() => {
                run.fg = cell.fg.pack();
            }
            _ => {
                if let Some(run) = current.take() {
                    runs.push(run);
                }
                current = Some(Run {
                    utf8_offset: text.len() as u32,
                    utf8_len: 0,
                    fg: cell.fg.pack(),
                    bg: cell.bg.pack(),
                    row,
                    col: col as u16,
                    cols: 0,
                    attrs: cell.attrs.bits(),
                });
                all_blank = true;
            }
        }

        let run = current.as_mut().expect("a run was just started");
        text.push_str(&cell.content);
        run.utf8_len += cell.content.len() as u32;
        run.cols += 1;
        all_blank &= blank;
        col += 1;
    }

    if let Some(run) = current {
        runs.push(run);
    }
}

/// Whether the cell at `col` is the first column of a double-width cluster:
/// content of its own, followed by the empty-content spacer the write path
/// leaves in the second column.
fn is_wide_base(cells: &[Cell], col: usize) -> bool {
    !cells[col].content.is_empty()
        && cells
            .get(col + 1)
            .is_some_and(|next| next.content.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cell::CellAttrs;
    use crate::color::Color;
    use crate::screen::Pen;

    fn screen(rows: u16, cols: u16) -> Screen {
        Screen::new(TerminalSize::new(rows, cols))
    }

    #[test]
    fn an_empty_screen_renders_no_runs() {
        let frame = screen(3, 10).render();
        assert!(frame.runs().is_empty());
        assert!(frame.text().is_empty());
        assert_eq!(frame.size(), TerminalSize::new(3, 10));
    }

    #[test]
    fn a_zero_sized_screen_renders_nothing() {
        let frame = screen(0, 0).render();
        assert!(frame.runs().is_empty());
    }

    #[test]
    fn plain_text_is_one_run_with_trailing_blanks_dropped() {
        let mut s = screen(2, 20);
        s.print("ls");
        let frame = s.render();
        assert_eq!(frame.runs().len(), 1);
        let run = frame.runs()[0];
        assert_eq!((run.row, run.col, run.cols), (0, 0, 2));
        assert_eq!(frame.run_text(&run), "ls");
        assert_eq!(frame.text(), "ls", "no bytes for the 18 trailing blanks");
    }

    #[test]
    fn interior_spaces_stay_inside_one_run() {
        let mut s = screen(1, 20);
        s.print("hello world");
        let frame = s.render();
        assert_eq!(frame.runs().len(), 1, "a space must not split a plain run");
        assert_eq!(frame.run_text(&frame.runs()[0]), "hello world");
    }

    #[test]
    fn leading_indentation_is_absorbed_by_the_text_that_follows() {
        let mut s = screen(1, 20);
        s.print("  ");
        s.pen_mut().fg = Color::RED;
        s.print("fn");
        let frame = s.render();
        assert_eq!(
            frame.runs().len(),
            1,
            "spaces show no foreground of their own"
        );
        let run = frame.runs()[0];
        assert_eq!((run.col, run.cols), (0, 4));
        assert_eq!(run.fg, Color::RED.pack());
        assert_eq!(frame.run_text(&run), "  fn");
    }

    #[test]
    fn a_style_change_starts_a_new_run() {
        let mut s = screen(1, 20);
        s.print("ab");
        *s.pen_mut() = Pen {
            fg: Color::RED,
            bg: Color::BLUE,
            attrs: CellAttrs::BOLD,
        };
        s.print("cd");
        let frame = s.render();
        assert_eq!(frame.runs().len(), 2);
        let [first, second] = [frame.runs()[0], frame.runs()[1]];
        assert_eq!(frame.run_text(&first), "ab");
        assert_eq!((first.fg, first.bg, first.attrs), (0, 0, 0));
        assert_eq!(frame.run_text(&second), "cd");
        assert_eq!(second.col, 2);
        assert_eq!(second.fg, Color::RED.pack());
        assert_eq!(second.bg, Color::BLUE.pack());
        assert_eq!(second.attrs, CellAttrs::BOLD.bits());
    }

    #[test]
    fn a_space_with_a_background_breaks_the_run_and_is_kept() {
        let mut s = screen(1, 20);
        s.print("a");
        s.pen_mut().bg = Color::BLUE;
        s.print(" ");
        s.pen_mut().bg = Color::Default;
        s.print("b");
        let frame = s.render();
        assert_eq!(frame.runs().len(), 3, "a coloured space is visible");
        assert_eq!(frame.runs()[1].bg, Color::BLUE.pack());
        assert_eq!(frame.run_text(&frame.runs()[1]), " ");
    }

    #[test]
    fn a_wide_grapheme_is_a_run_of_its_own() {
        // The frontend maps a glyph back to its column by counting clusters,
        // which only works while every cluster in a run is one column wide. So
        // a double-width cluster is never merged with its neighbours.
        let mut s = screen(1, 10);
        s.print("a漢b");
        let frame = s.render();
        assert_eq!(frame.runs().len(), 3);

        let [before, wide, after] = [frame.runs()[0], frame.runs()[1], frame.runs()[2]];
        assert_eq!((before.col, before.cols), (0, 1));
        assert_eq!(frame.run_text(&before), "a");
        assert_eq!((wide.col, wide.cols), (1, 2), "two columns for one cluster");
        assert_eq!(
            frame.run_text(&wide),
            "漢",
            "the spacer contributes no bytes"
        );
        assert_eq!((after.col, after.cols), (3, 1));
        assert_eq!(frame.run_text(&after), "b");

        assert_eq!(frame.row_text(0), "a漢b", "the row still reads whole");
    }

    #[test]
    fn a_wide_grapheme_keeps_the_style_of_the_cell_it_came_from() {
        let mut s = screen(1, 10);
        s.pen_mut().fg = Color::RED;
        s.pen_mut().attrs.insert(CellAttrs::BOLD);
        s.print("漢");
        let frame = s.render();
        let wide = frame.runs()[0];
        assert_eq!(wide.fg, Color::RED.pack());
        assert_eq!(wide.attrs, CellAttrs::BOLD.bits());
        assert_eq!(wide.cols, 2);
    }

    #[test]
    fn adjacent_wide_graphemes_stay_separate() {
        let mut s = screen(1, 10);
        s.print("漢字");
        let frame = s.render();
        assert_eq!(frame.runs().len(), 2);
        assert_eq!(frame.runs()[0].col, 0);
        assert_eq!(frame.runs()[1].col, 2);
        assert!(frame.runs().iter().all(|r| r.cols == 2));
    }

    #[test]
    fn a_combining_mark_stays_with_its_base_character() {
        let mut s = screen(1, 10);
        s.print("e\u{301}x");
        let frame = s.render();
        let run = frame.runs()[0];
        assert_eq!(run.cols, 2, "the mark shares its base character's column");
        assert_eq!(frame.run_text(&run), "e\u{301}x");
    }

    #[test]
    fn runs_are_grouped_by_row_and_addressable_per_row() {
        let mut s = screen(3, 10);
        s.print("one");
        s.carriage_return();
        s.line_feed();
        s.line_feed();
        s.print("three");
        let frame = s.render();
        assert_eq!(frame.row_runs(0).len(), 1);
        assert_eq!(frame.row_runs(1).len(), 0, "the middle row is blank");
        assert_eq!(frame.row_runs(2).len(), 1);
        assert_eq!(frame.row_text(0), "one");
        assert_eq!(frame.row_text(1), "");
        assert_eq!(frame.row_text(2), "three");
        assert_eq!(frame.row_runs(9), &[], "a row out of range has no runs");
    }

    #[test]
    fn a_gap_between_runs_keeps_its_columns() {
        let mut s = screen(1, 20);
        s.print("a");
        s.cursor_to(0, 5);
        s.pen_mut().bg = Color::BLUE;
        s.print("b");
        let frame = s.render();
        // The blanks in between are default-styled, so they are dropped from
        // the text and re-derived from the columns.
        assert_eq!(frame.row_text(0), "a    b");
        assert_eq!(frame.runs()[1].col, 5);
    }

    #[test]
    fn offsets_slice_the_shared_buffer_in_order() {
        let mut s = screen(2, 10);
        s.print("ab");
        s.carriage_return();
        s.line_feed();
        s.print("cd");
        let frame = s.render();
        assert_eq!(frame.text(), "abcd", "one buffer for the whole frame");
        assert_eq!(frame.runs()[1].utf8_offset, 2);
        for run in frame.runs() {
            let end = run.utf8_offset + run.utf8_len;
            assert!(end as usize <= frame.text().len(), "run stays in bounds");
        }
    }

    #[test]
    fn the_cursor_travels_with_the_frame() {
        let mut s = screen(3, 10);
        s.print("abc");
        let frame = s.render();
        assert_eq!(frame.cursor(), Position::new(0, 3));
    }

    #[test]
    fn hiding_the_cursor_travels_with_the_frame() {
        let mut s = screen(3, 10);
        assert!(s.render().cursor_visible(), "visible by default");
        let mut p = crate::parsers::vt::VtParser::new();
        let _ = s.advance(&mut p, b"\x1b[?25l");
        assert!(!s.render().cursor_visible());
    }

    #[test]
    fn rendering_into_a_reused_frame_matches_a_fresh_one() {
        let mut s = screen(2, 10);
        s.print("first");
        let mut frame = Frame::new();
        s.render_into(&mut frame);

        s.carriage_return();
        s.print("second line is longer");
        s.render_into(&mut frame);
        assert_eq!(frame, s.render(), "a reused frame holds no stale state");
    }

    #[test]
    fn a_run_is_twenty_four_bytes_with_no_padding() {
        // The FFI copies these by the arrayful; a surprise in the layout would
        // be a surprise in the header cbindgen generates (PRD §14).
        assert_eq!(std::mem::size_of::<Run>(), 24);
        assert_eq!(std::mem::align_of::<Run>(), 4);
    }
}
