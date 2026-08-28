#include "Metrics.h"

#include <algorithm>
#include <cmath>

namespace glue {

CellSize cell_size(double advance, double ascent, double descent, double leading) {
    CellSize cell;
    // A font that reports nonsense must not produce a zero-width grid, which
    // would divide by zero one caller later.
    cell.width = std::max(1.0, std::round(advance));
    cell.ascent = std::max(0.0, ascent);
    const double line = std::max(0.0, ascent) + std::max(0.0, descent) + std::max(0.0, leading);
    cell.height = std::max(1.0, std::ceil(line));
    return cell;
}

GridSize grid_for(double view_width, double view_height, CellSize cell) {
    GridSize grid;
    const double cols = std::floor(view_width / cell.width);
    const double rows = std::floor(view_height / cell.height);
    // Clamp at both ends: below by 1, above by what a u16 can carry, which is
    // also what the engine's TerminalSize can carry.
    grid.cols = static_cast<std::uint16_t>(std::clamp(cols, 1.0, 65535.0));
    grid.rows = static_cast<std::uint16_t>(std::clamp(rows, 1.0, 65535.0));
    return grid;
}

double column_x(std::uint16_t col, CellSize cell) { return col * cell.width; }

double baseline_y(std::uint16_t row, CellSize cell, double view_height) {
    // Unflipped coordinates: y counts up from the bottom, so row 0 sits one
    // cell below the top edge and the baseline is `ascent` below that.
    return view_height - (row * cell.height) - cell.ascent;
}

Rect cell_rect(std::uint16_t row, std::uint16_t col, std::uint16_t cols, CellSize cell,
               double view_height) {
    Rect rect;
    rect.x = column_x(col, cell);
    rect.y = view_height - ((row + 1) * cell.height);
    rect.width = cols * cell.width;
    rect.height = cell.height;
    return rect;
}

}  // namespace glue
