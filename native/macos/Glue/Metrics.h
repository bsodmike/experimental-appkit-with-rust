// The cell grid: turning a font's measurements and a view's size into the
// coordinates everything else is drawn at.
//
// PRD-mac §4. There is no AppKit here on purpose: this is the arithmetic that
// decides whether a terminal looks like a grid or like a slowly bending column
// of text, and it is worth being able to test it without a screen.

#pragma once

#include <cstdint>

namespace glue {

/// The size of one character cell, in points.
struct CellSize {
    /// Rounded to whole points, once, so that every column is placed from an
    /// exact multiple and no rounding error accumulates across a row.
    double width = 1.0;
    double height = 1.0;
    /// Distance from the top of the cell down to the text baseline.
    double ascent = 1.0;
};

/// The terminal size a view can hold.
struct GridSize {
    std::uint16_t rows = 1;
    std::uint16_t cols = 1;
};

/// A rectangle in the view's own (unflipped, origin bottom-left) coordinates.
struct Rect {
    double x = 0.0;
    double y = 0.0;
    double width = 0.0;
    double height = 0.0;
};

/// Derive the cell from a font's metrics.
///
/// `advance` is the horizontal advance of a representative glyph — for a
/// monospace font, any of them. The width is *rounded* rather than truncated or
/// ceilinged: half a point of extra tracking per column is invisible, a whole
/// point is not.
CellSize cell_size(double advance, double ascent, double descent, double leading);

/// How many rows and columns fit in a view of this size. Always at least 1x1:
/// a terminal with no cells has nowhere to put the cursor, and every caller
/// would need the same guard.
GridSize grid_for(double view_width, double view_height, CellSize cell);

/// The x of a column's left edge.
double column_x(std::uint16_t col, CellSize cell);

/// The y of a text baseline for a display row, in unflipped coordinates: row 0
/// is at the top of the view, which is where the terminal puts it.
double baseline_y(std::uint16_t row, CellSize cell, double view_height);

/// The rectangle covering `cols` columns starting at `(row, col)` — what a run's
/// background is filled with, and what the cursor is drawn into.
Rect cell_rect(std::uint16_t row, std::uint16_t col, std::uint16_t cols, CellSize cell,
               double view_height);

}  // namespace glue
