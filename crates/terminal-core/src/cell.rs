//! The single character cell, and the display attributes it carries.
//!
//! ## A note on content
//!
//! A [`Cell`] stores its character as a [`CompactString`] holding one UTF-8
//! *grapheme cluster*. This is what lets a cell represent combining marks
//! (`e` + U+0301) and emoji ZWJ sequences, which PRD §10 requires and a single
//! `char` cannot hold. A lone space denotes an empty cell.
//!
//! `CompactString` stores clusters up to 24 bytes inline with no heap
//! allocation (PRD §17.5, #19), which is every real grapheme cluster. Bulk
//! scrollback is stored separately as packed UTF-8 text plus attribute runs
//! (PRD §17.5, #18), not as `Cell`s, so this type only ever backs the bounded
//! active grid.
//!
//! One thing is deliberately still open: **the wide-character spacer.** A
//! double-width grapheme occupies two columns with a trailing spacer cell
//! (PRD §10). How that spacer is marked arrives with wide-character handling in
//! the grid, not here.

use crate::color::Color;
use compact_str::CompactString;

/// A bit-set of display attributes for one cell.
///
/// Backed by a `u16` to match the `attrs` field of the render run that crosses
/// the FFI boundary (PRD §10), so the engine's representation and the wire
/// representation never need translating. Hand-rolled rather than pulling in a
/// bitflags dependency; the surface is small and can be swapped later without
/// touching call sites.
#[derive(Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct CellAttrs(u16);

impl CellAttrs {
    pub const EMPTY: Self = Self(0);

    pub const BOLD: Self = Self(1 << 0);
    pub const DIM: Self = Self(1 << 1);
    pub const ITALIC: Self = Self(1 << 2);
    pub const UNDERLINE: Self = Self(1 << 3);
    pub const BLINK: Self = Self(1 << 4);
    pub const REVERSE: Self = Self(1 << 5);
    pub const HIDDEN: Self = Self(1 << 6);
    pub const STRIKETHROUGH: Self = Self(1 << 7);

    /// The raw bits, e.g. to copy into a render run.
    pub const fn bits(self) -> u16 {
        self.0
    }

    /// Whether *every* attribute in `other` is set in `self`.
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Whether no attributes are set.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Add the attributes in `other`.
    pub fn insert(&mut self, other: Self) {
        self.0 |= other.0;
    }

    /// Remove the attributes in `other`.
    pub fn remove(&mut self, other: Self) {
        self.0 &= !other.0;
    }
}

impl std::ops::BitOr for CellAttrs {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl std::fmt::Debug for CellAttrs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Render as the set of named flags rather than an opaque integer.
        const NAMED: &[(CellAttrs, &str)] = &[
            (CellAttrs::BOLD, "BOLD"),
            (CellAttrs::DIM, "DIM"),
            (CellAttrs::ITALIC, "ITALIC"),
            (CellAttrs::UNDERLINE, "UNDERLINE"),
            (CellAttrs::BLINK, "BLINK"),
            (CellAttrs::REVERSE, "REVERSE"),
            (CellAttrs::HIDDEN, "HIDDEN"),
            (CellAttrs::STRIKETHROUGH, "STRIKETHROUGH"),
        ];
        if self.is_empty() {
            return write!(f, "CellAttrs(EMPTY)");
        }
        write!(f, "CellAttrs(")?;
        let mut first = true;
        for (flag, name) in NAMED {
            if self.contains(*flag) {
                if !first {
                    write!(f, " | ")?;
                }
                write!(f, "{name}")?;
                first = false;
            }
        }
        write!(f, ")")
    }
}

/// One character cell of the grid.
///
/// `content` holds a single UTF-8 grapheme cluster (see the module note). A
/// lone space denotes an empty cell.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Cell {
    /// The grapheme cluster shown in this cell, as UTF-8. A single space
    /// denotes an empty cell.
    pub content: CompactString,
    pub fg: Color,
    pub bg: Color,
    pub attrs: CellAttrs,
}

impl Cell {
    /// An empty cell: a space with default colours and no attributes. This is
    /// what a freshly cleared or newly allocated grid position holds.
    pub const fn blank() -> Self {
        Self {
            content: CompactString::const_new(" "),
            fg: Color::Default,
            bg: Color::Default,
            attrs: CellAttrs::EMPTY,
        }
    }

    /// Whether this cell would render as blank: a space with no background or
    /// attributes that could make the space itself visible.
    ///
    /// The foreground is intentionally ignored — a space's foreground is never
    /// drawn — but a non-default background or an attribute like `REVERSE` or
    /// `UNDERLINE` *is* visible on a space, so those make the cell non-blank.
    pub fn is_blank(&self) -> bool {
        self.content == " " && self.bg.is_default() && self.attrs.is_empty()
    }
}

impl Default for Cell {
    fn default() -> Self {
        Self::blank()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_cell_is_blank() {
        assert!(Cell::blank().is_blank());
        assert_eq!(Cell::default(), Cell::blank());
    }

    #[test]
    fn a_space_with_visible_decoration_is_not_blank() {
        let mut c = Cell::blank();
        c.bg = Color::RED;
        assert!(!c.is_blank(), "coloured background is visible on a space");

        let mut c = Cell::blank();
        c.attrs.insert(CellAttrs::UNDERLINE);
        assert!(!c.is_blank(), "underline is visible on a space");
    }

    #[test]
    fn a_space_with_only_a_foreground_is_still_blank() {
        let mut c = Cell::blank();
        c.fg = Color::RED;
        assert!(c.is_blank(), "foreground alone never renders on a space");
    }

    #[test]
    fn a_cell_holds_a_multi_byte_grapheme_cluster() {
        // Base letter plus a combining acute accent: a single grapheme made of
        // two scalar values, which a `char` could never have held.
        let mut c = Cell::blank();
        c.content = CompactString::const_new("e\u{301}");
        assert_eq!(c.content.chars().count(), 2);
        assert!(!c.is_blank());
    }

    #[test]
    fn attrs_insert_and_remove_are_inverses() {
        let mut a = CellAttrs::EMPTY;
        a.insert(CellAttrs::BOLD | CellAttrs::ITALIC);
        assert!(a.contains(CellAttrs::BOLD));
        assert!(a.contains(CellAttrs::ITALIC));
        assert!(a.contains(CellAttrs::BOLD | CellAttrs::ITALIC));
        assert!(!a.contains(CellAttrs::UNDERLINE));

        a.remove(CellAttrs::BOLD);
        assert!(!a.contains(CellAttrs::BOLD));
        assert!(a.contains(CellAttrs::ITALIC));
    }

    #[test]
    fn contains_requires_all_queried_flags() {
        let a = CellAttrs::BOLD;
        // Not all of {BOLD, ITALIC} are present, so contains is false.
        assert!(!a.contains(CellAttrs::BOLD | CellAttrs::ITALIC));
    }

    #[test]
    fn attrs_fit_in_the_render_run_width() {
        // All named flags must live in the low bits of the u16 that crosses the
        // boundary. STRIKETHROUGH is the highest bit currently defined.
        assert_eq!(CellAttrs::STRIKETHROUGH.bits(), 1 << 7);
    }

    #[test]
    fn debug_lists_named_flags() {
        assert_eq!(format!("{:?}", CellAttrs::EMPTY), "CellAttrs(EMPTY)");
        assert_eq!(
            format!("{:?}", CellAttrs::BOLD | CellAttrs::UNDERLINE),
            "CellAttrs(BOLD | UNDERLINE)"
        );
    }
}
