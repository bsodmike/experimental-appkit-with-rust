//! A rectangular buffer of [`Cell`]s.
//!
//! This is a deliberately *dumb* primitive: a fixed grid you can index, clear,
//! fill and resize. It knows nothing about scrollback, reflow, the cursor, or
//! grapheme segmentation, and it makes no claim to be "the primary screen."
//!
//! That restraint is intentional. PRD §16.1 decides that scrollback is stored
//! as *logical lines* and the primary screen's display rows are *derived
//! indices* into them — not an owned grid. So this type is **not** the primary
//! buffer. What it genuinely is:
//!
//! - the **alternate screen**, which never reflows (§16.1) and so really is a
//!   fixed rectangle of cells;
//! - a render / scratch target;
//! - the substrate the derived-display-row layer will read into later.
//!
//! Writing *into* the grid with terminal semantics — advancing the cursor,
//! splitting graphemes, placing wide-character spacers — lives with the cursor
//! and parser, not here. Here you only get whole-[`Cell`] placement.

use crate::cell::Cell;
use crate::geometry::{Position, TerminalSize};

/// A fixed-size, row-major rectangle of cells.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Grid {
    size: TerminalSize,
    /// Row-major: the cell at `(row, col)` lives at `row * cols + col`. Always
    /// exactly `size.cell_count()` long.
    cells: Vec<Cell>,
}

impl Grid {
    /// A new grid of the given size, every cell [`Cell::blank`].
    pub fn new(size: TerminalSize) -> Self {
        Self {
            size,
            cells: vec![Cell::blank(); size.cell_count()],
        }
    }

    /// The grid's dimensions.
    pub fn size(&self) -> TerminalSize {
        self.size
    }

    /// The flat index of a position, if it is in bounds.
    fn index(&self, pos: Position) -> Option<usize> {
        if pos.is_within(self.size) {
            Some(pos.row as usize * self.size.cols as usize + pos.col as usize)
        } else {
            None
        }
    }

    /// The cell at `pos`, or `None` if out of bounds.
    pub fn get(&self, pos: Position) -> Option<&Cell> {
        self.index(pos).map(|i| &self.cells[i])
    }

    /// A mutable reference to the cell at `pos`, or `None` if out of bounds.
    pub fn get_mut(&mut self, pos: Position) -> Option<&mut Cell> {
        match self.index(pos) {
            Some(i) => Some(&mut self.cells[i]),
            None => None,
        }
    }

    /// Place `cell` at `pos`. Returns `false` (and does nothing) if `pos` is out
    /// of bounds — writes past the edge are dropped, never a panic.
    pub fn set(&mut self, pos: Position, cell: Cell) -> bool {
        match self.index(pos) {
            Some(i) => {
                self.cells[i] = cell;
                true
            }
            None => false,
        }
    }

    /// One row as a contiguous slice, or `None` if `row` is out of range. This
    /// is the shape the copy-out / render path wants: a whole line at once.
    pub fn row(&self, row: u16) -> Option<&[Cell]> {
        if row >= self.size.rows {
            return None;
        }
        let cols = self.size.cols as usize;
        let start = row as usize * cols;
        Some(&self.cells[start..start + cols])
    }

    /// Iterate the rows top to bottom, each as a contiguous slice.
    pub fn rows(&self) -> impl Iterator<Item = &[Cell]> {
        // Driven by row index rather than `chunks`, which would panic on a
        // zero-column grid.
        (0..self.size.rows).map(move |r| self.row(r).expect("row index in range"))
    }

    /// Reset every cell to [`Cell::blank`].
    pub fn clear(&mut self) {
        self.fill(Cell::blank());
    }

    /// Set every cell to a clone of `cell`.
    pub fn fill(&mut self, cell: Cell) {
        for slot in &mut self.cells {
            *slot = cell.clone();
        }
    }

    /// Resize the grid, anchoring existing content to the top-left corner:
    /// cells within the overlap of the old and new dimensions are preserved,
    /// newly exposed cells are blank, and cells outside the new bounds are
    /// dropped.
    ///
    /// This is a plain rectangular resize, **not** reflow. Wrapped-line reflow
    /// is a property of the logical-line buffer (PRD §16), not of this grid;
    /// the alternate screen — this grid's real use — is defined to *not* reflow
    /// (§16.1), so top-left anchoring is the correct neutral behaviour here.
    pub fn resize(&mut self, new_size: TerminalSize) {
        if new_size == self.size {
            return;
        }
        let mut new_cells = vec![Cell::blank(); new_size.cell_count()];
        let copy_rows = self.size.rows.min(new_size.rows) as usize;
        let copy_cols = self.size.cols.min(new_size.cols) as usize;
        for r in 0..copy_rows {
            let old_base = r * self.size.cols as usize;
            let new_base = r * new_size.cols as usize;
            new_cells[new_base..new_base + copy_cols]
                .clone_from_slice(&self.cells[old_base..old_base + copy_cols]);
        }
        self.cells = new_cells;
        self.size = new_size;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cell::CellAttrs;
    use crate::color::Color;

    fn cell(content: &str) -> Cell {
        Cell {
            content: content.to_string(),
            ..Cell::blank()
        }
    }

    #[test]
    fn new_grid_is_sized_and_all_blank() {
        let g = Grid::new(TerminalSize::new(3, 4));
        assert_eq!(g.size(), TerminalSize::new(3, 4));
        assert!(g.rows().flatten().all(Cell::is_blank));
        assert_eq!(g.rows().count(), 3);
        assert_eq!(g.row(0).unwrap().len(), 4);
    }

    #[test]
    fn get_and_set_round_trip_in_bounds() {
        let mut g = Grid::new(TerminalSize::new(2, 2));
        let pos = Position::new(1, 0);
        assert!(g.set(pos, cell("x")));
        assert_eq!(g.get(pos).unwrap().content, "x");
        // A neighbour is untouched.
        assert!(g.get(Position::new(0, 0)).unwrap().is_blank());
    }

    #[test]
    fn out_of_bounds_access_is_none_and_set_is_a_noop() {
        let mut g = Grid::new(TerminalSize::new(2, 2));
        let outside = Position::new(2, 0);
        assert!(g.get(outside).is_none());
        assert!(g.get_mut(outside).is_none());
        assert!(!g.set(outside, cell("x")), "write past the edge is dropped");
        assert!(g.rows().flatten().all(Cell::is_blank));
    }

    #[test]
    fn get_mut_allows_in_place_edit() {
        let mut g = Grid::new(TerminalSize::new(1, 1));
        let c = g.get_mut(Position::new(0, 0)).unwrap();
        c.content = "q".to_string();
        c.attrs.insert(CellAttrs::BOLD);
        c.fg = Color::RED;
        let c = g.get(Position::new(0, 0)).unwrap();
        assert_eq!(c.content, "q");
        assert!(c.attrs.contains(CellAttrs::BOLD));
    }

    #[test]
    fn fill_then_clear() {
        let mut g = Grid::new(TerminalSize::new(2, 3));
        g.fill(cell("#"));
        assert!(g.rows().flatten().all(|c| c.content == "#"));
        g.clear();
        assert!(g.rows().flatten().all(Cell::is_blank));
    }

    #[test]
    fn resize_larger_preserves_top_left_and_blanks_the_rest() {
        let mut g = Grid::new(TerminalSize::new(1, 1));
        g.set(Position::new(0, 0), cell("a"));
        g.resize(TerminalSize::new(2, 2));
        assert_eq!(g.size(), TerminalSize::new(2, 2));
        assert_eq!(g.get(Position::new(0, 0)).unwrap().content, "a");
        // Newly exposed cells are blank.
        assert!(g.get(Position::new(0, 1)).unwrap().is_blank());
        assert!(g.get(Position::new(1, 0)).unwrap().is_blank());
        assert!(g.get(Position::new(1, 1)).unwrap().is_blank());
    }

    #[test]
    fn resize_smaller_drops_cells_outside_new_bounds() {
        let mut g = Grid::new(TerminalSize::new(2, 2));
        g.set(Position::new(0, 0), cell("a"));
        g.set(Position::new(1, 1), cell("d")); // outside the shrunk grid
        g.resize(TerminalSize::new(1, 1));
        assert_eq!(g.size(), TerminalSize::new(1, 1));
        assert_eq!(g.get(Position::new(0, 0)).unwrap().content, "a");
        assert!(g.get(Position::new(1, 1)).is_none());
    }

    #[test]
    fn resize_preserves_row_major_layout_not_flat_order() {
        // A 2x3 grid whose second row is marked; after widening to 2x4 the
        // marks must still be on row 1, i.e. cells move by row, not by flat
        // index. This is the bug a naive Vec::resize would introduce.
        let mut g = Grid::new(TerminalSize::new(2, 3));
        for c in 0..3 {
            g.set(Position::new(1, c), cell("*"));
        }
        g.resize(TerminalSize::new(2, 4));
        assert!(g.row(0).unwrap().iter().all(Cell::is_blank));
        for c in 0..3 {
            assert_eq!(g.get(Position::new(1, c)).unwrap().content, "*");
        }
        assert!(g.get(Position::new(1, 3)).unwrap().is_blank());
    }

    #[test]
    fn zero_column_grid_has_no_cells_but_still_reports_rows() {
        let g = Grid::new(TerminalSize::new(3, 0));
        assert_eq!(g.rows().count(), 3);
        assert!(g.rows().all(|r| r.is_empty()));
        assert!(g.get(Position::new(0, 0)).is_none());
    }
}
