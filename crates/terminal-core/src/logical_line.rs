//! A packed logical line: the immutable, width-independent unit of scrollback.
//!
//! PRD §17.5 #18: scrollback stores logical lines as contiguous UTF-8 text plus
//! attribute runs, not as `Cell`s. This is compact (a few MB at the 100k
//! ceiling instead of hundreds), and it is already the shape the render path
//! wants (PRD §10), so copy-out is nearly a memcpy. A logical line is frozen
//! once built, so packing costs nothing in mutation.
//!
//! Display rows are *derived* by wrapping the text to the current width; the
//! result is cached per line (#23). Offsets are byte offsets into `text`,
//! grapheme-aligned (#20).

use crate::cell::{Cell, CellAttrs};
use crate::color::Color;
use crate::text::{grapheme_indices, grapheme_width};

/// A monotonic, never-reused identifier for a logical line (PRD §16.2, #12).
/// Allocation of new ids is the buffer's responsibility, not the line's.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct LineId(pub u64);

/// A run of consecutive text sharing one style, addressed by byte range into
/// the owning line's `text`. Runs partition the text contiguously. Colours are
/// symbolic (`Color`, not a resolved `u32`); resolution happens at FFI copy-out.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct AttrRun {
    pub byte_start: u32,
    pub byte_len: u32,
    pub fg: Color,
    pub bg: Color,
    pub attrs: CellAttrs,
}

/// Cached wrapping of a line to a specific width.
#[derive(Clone, PartialEq, Eq, Debug)]
struct WrapCache {
    width: u16,
    /// Byte offsets where display rows 2..N begin; row 1 is implicitly at 0.
    /// Empty for a line that fits on one display row, so a `Vec` that never
    /// grows performs no heap allocation — the common short-line case (#23).
    continuation_starts: Vec<u32>,
}

/// A frozen logical line stored in scrollback.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LogicalLine {
    id: LineId,
    text: String,
    runs: Vec<AttrRun>,
    wrap_cache: Option<WrapCache>,
}

impl LogicalLine {
    /// Pack a run of active-grid cells into a frozen logical line. Trailing
    /// blank cells are dropped (they carry nothing visible); cells sharing a
    /// style coalesce into one run.
    pub fn from_cells(id: LineId, cells: &[Cell]) -> Self {
        let end = cells
            .iter()
            .rposition(|c| !c.is_blank())
            .map_or(0, |i| i + 1);
        let cells = &cells[..end];

        let mut text = String::new();
        let mut runs: Vec<AttrRun> = Vec::new();
        for cell in cells {
            let start = text.len() as u32;
            text.push_str(&cell.content);
            let len = text.len() as u32 - start;
            if let Some(last) = runs.last_mut()
                && last.fg == cell.fg
                && last.bg == cell.bg
                && last.attrs == cell.attrs
            {
                last.byte_len += len;
                continue;
            }
            runs.push(AttrRun {
                byte_start: start,
                byte_len: len,
                fg: cell.fg,
                bg: cell.bg,
                attrs: cell.attrs,
            });
        }
        Self {
            id,
            text,
            runs,
            wrap_cache: None,
        }
    }

    pub fn id(&self) -> LineId {
        self.id
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn runs(&self) -> &[AttrRun] {
        &self.runs
    }

    /// The number of bytes of packed text, used to anchor split lines against
    /// the concatenated whole (#24).
    pub fn byte_len(&self) -> u32 {
        self.text.len() as u32
    }

    /// The byte offsets at which display rows 2..N begin when wrapped to
    /// `width` (row 1 is implicitly byte 0). Cached; recomputed only when the
    /// requested width differs from the cached one (#23).
    pub fn wrap(&mut self, width: u16) -> &[u32] {
        let stale = !matches!(&self.wrap_cache, Some(c) if c.width == width);
        if stale {
            let continuation_starts = compute_continuation_starts(&self.text, width);
            self.wrap_cache = Some(WrapCache {
                width,
                continuation_starts,
            });
        }
        &self.wrap_cache.as_ref().unwrap().continuation_starts
    }

    /// How many display rows this line occupies at `width` (always at least 1).
    pub fn display_row_count(&mut self, width: u16) -> usize {
        self.wrap(width).len() + 1
    }
}

/// Scan the text accumulating grapheme display widths, returning the byte
/// offset at which each display row after the first begins. A wide grapheme is
/// never split across a wrap: if it would overflow the row it wraps before,
/// leaving a padding column (PRD §16.3).
fn compute_continuation_starts(text: &str, width: u16) -> Vec<u32> {
    let mut starts = Vec::new();
    if width == 0 {
        return starts;
    }
    let mut col = 0u16;
    for (byte_idx, cluster) in grapheme_indices(text) {
        let w = grapheme_width(cluster);
        if col > 0 && col + w > width {
            starts.push(byte_idx as u32);
            col = 0;
        }
        col += w;
    }
    starts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn styled(content: &str, fg: Color) -> Cell {
        Cell {
            content: content.into(),
            fg,
            ..Cell::blank()
        }
    }

    fn line_of(id: u64, s: &str) -> LogicalLine {
        let cells: Vec<Cell> = s
            .chars()
            .map(|c| styled(&c.to_string(), Color::Default))
            .collect();
        LogicalLine::from_cells(LineId(id), &cells)
    }

    #[test]
    fn from_cells_packs_text_and_preserves_id() {
        let line = line_of(7, "hi");
        assert_eq!(line.id(), LineId(7));
        assert_eq!(line.text(), "hi");
    }

    #[test]
    fn trailing_blank_cells_are_trimmed() {
        let cells = [
            styled("h", Color::Default),
            styled("i", Color::Default),
            Cell::blank(),
            Cell::blank(),
        ];
        let line = LogicalLine::from_cells(LineId(0), &cells);
        assert_eq!(line.text(), "hi");
    }

    #[test]
    fn a_colored_trailing_space_is_not_trimmed() {
        let mut space = Cell::blank();
        space.bg = Color::RED;
        let cells = [styled("h", Color::Default), space];
        let line = LogicalLine::from_cells(LineId(0), &cells);
        assert_eq!(line.text(), "h ");
    }

    #[test]
    fn same_style_cells_coalesce_into_one_run() {
        let line = line_of(0, "abc");
        assert_eq!(line.runs().len(), 1);
        assert_eq!(line.runs()[0].byte_len, 3);
    }

    #[test]
    fn style_changes_break_runs() {
        let cells = [
            styled("a", Color::RED),
            styled("b", Color::RED),
            styled("c", Color::BLUE),
        ];
        let line = LogicalLine::from_cells(LineId(0), &cells);
        assert_eq!(line.runs().len(), 2);
        assert_eq!(line.runs()[0].byte_start, 0);
        assert_eq!(line.runs()[1].byte_start, 2);
    }

    #[test]
    fn a_short_line_is_one_display_row_with_no_heap_wrap() {
        let mut line = line_of(0, "hello");
        assert_eq!(line.wrap(80), &[] as &[u32]);
        assert_eq!(line.display_row_count(80), 1);
    }

    #[test]
    fn wrapping_records_continuation_offsets() {
        let mut line = line_of(0, "abcdef");
        assert_eq!(line.wrap(3), &[3]);
        assert_eq!(line.display_row_count(3), 2);
        assert_eq!(line.wrap(2), &[2, 4]);
        assert_eq!(line.display_row_count(2), 3);
    }

    #[test]
    fn wide_graphemes_are_not_split_across_a_wrap() {
        // Three width-2 CJK chars (3 bytes each) at width 3: one per row, each
        // leaving a padding column.
        let mut line = line_of(0, "\u{4E2D}\u{4E2D}\u{4E2D}");
        assert_eq!(line.wrap(3), &[3, 6]);
        assert_eq!(line.display_row_count(3), 3);
    }

    #[test]
    fn wrap_cache_recomputes_only_on_width_change() {
        let mut line = line_of(0, "abcdef");
        assert_eq!(line.wrap(3), &[3]);
        // Same width: still correct (served from cache).
        assert_eq!(line.wrap(3), &[3]);
        // New width: recomputed.
        assert_eq!(line.wrap(2), &[2, 4]);
    }

    #[test]
    fn empty_line_is_one_display_row() {
        let mut line = LogicalLine::from_cells(LineId(0), &[]);
        assert_eq!(line.text(), "");
        assert_eq!(line.display_row_count(80), 1);
    }
}
