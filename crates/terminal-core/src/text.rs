//! The single authority for grapheme segmentation and display width.
//!
//! PRD §17.5 #25: the write path and the reflow scan must segment text into
//! cells and compute column widths *identically*, or the display desyncs (the
//! application advances the cursor by two while the engine wraps as if by one).
//! So both go through the functions here and nowhere else.
//!
//! Width policy: `unicode-width`'s narrow (`width`, not `width_cjk`) reading, so
//! East Asian *ambiguous* characters are width 1 by default (configurable
//! post-MVP), with terminal-specific overrides layered on for variation
//! selectors, ZWJ emoji sequences and regional-indicator flags, which
//! `unicode-width` alone does not resolve to a single glyph.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthChar;

const ZWJ: char = '\u{200D}';
const VS16_EMOJI: char = '\u{FE0F}';
const VS15_TEXT: char = '\u{FE0E}';

fn is_regional_indicator(c: char) -> bool {
    ('\u{1F1E6}'..='\u{1F1FF}').contains(&c)
}

/// Split a string into extended grapheme clusters (UAX #29) — one per cell.
pub fn graphemes(s: &str) -> impl Iterator<Item = &str> {
    UnicodeSegmentation::graphemes(s, true)
}

/// Like [`graphemes`], but yielding each cluster's starting byte offset too.
pub fn grapheme_indices(s: &str) -> impl Iterator<Item = (usize, &str)> {
    UnicodeSegmentation::grapheme_indices(s, true)
}

/// The column width of one grapheme cluster: 0 (zero-width), 1 (normal) or 2
/// (wide). Combining marks contribute nothing because width is taken from the
/// cluster's base; the emoji/flag rules override where `unicode-width`'s
/// per-scalar sum would be wrong.
pub fn grapheme_width(cluster: &str) -> u16 {
    let Some(base) = cluster.chars().next() else {
        return 0;
    };
    if cluster.contains(VS16_EMOJI) {
        return 2;
    }
    if cluster.contains(VS15_TEXT) {
        return 1;
    }
    if cluster.contains(ZWJ) {
        return 2;
    }
    if is_regional_indicator(base)
        && cluster
            .chars()
            .filter(|c| is_regional_indicator(*c))
            .count()
            >= 2
    {
        return 2;
    }
    base.width().unwrap_or(0).min(2) as u16
}

/// The total column width of a string, summing its grapheme widths.
pub fn display_width(s: &str) -> usize {
    graphemes(s).map(|g| grapheme_width(g) as usize).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_is_width_one() {
        assert_eq!(grapheme_width("a"), 1);
        assert_eq!(grapheme_width(" "), 1);
    }

    #[test]
    fn cjk_ideograph_is_wide() {
        // U+4E2D, a full-width CJK character.
        assert_eq!(grapheme_width("\u{4E2D}"), 2);
    }

    #[test]
    fn combining_mark_does_not_add_width() {
        // "e" + combining acute: one grapheme, one column.
        let cluster = "e\u{301}";
        assert_eq!(graphemes(cluster).count(), 1);
        assert_eq!(grapheme_width(cluster), 1);
    }

    #[test]
    fn variation_selector_16_forces_emoji_width() {
        // U+2764 (heart) defaults to text width 1; + VS16 makes it emoji, width 2.
        assert_eq!(grapheme_width("\u{2764}"), 1);
        assert_eq!(grapheme_width("\u{2764}\u{FE0F}"), 2);
    }

    #[test]
    fn variation_selector_15_forces_text_width() {
        assert_eq!(grapheme_width("\u{2764}\u{FE0E}"), 1);
    }

    #[test]
    fn regional_indicator_pair_is_one_flag_of_width_two() {
        // U+1F1FA U+1F1F8: one flag, one grapheme, width 2.
        let flag = "\u{1F1FA}\u{1F1F8}";
        assert_eq!(graphemes(flag).count(), 1);
        assert_eq!(grapheme_width(flag), 2);
    }

    #[test]
    fn zwj_sequence_is_a_single_wide_glyph() {
        // Family: man ZWJ woman ZWJ girl. One grapheme, width 2 (not the sum).
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
        assert_eq!(graphemes(family).count(), 1);
        assert_eq!(grapheme_width(family), 2);
    }

    #[test]
    fn ambiguous_width_defaults_to_narrow() {
        // U+00A1 is East Asian Ambiguous; the narrow policy makes it width 1.
        assert_eq!(grapheme_width("\u{00A1}"), 1);
    }

    #[test]
    fn zero_width_space_is_width_zero() {
        assert_eq!(grapheme_width("\u{200B}"), 0);
    }

    #[test]
    fn graphemes_splits_mixed_text() {
        let parts: Vec<&str> = graphemes("a\u{4E2D}e\u{301}").collect();
        assert_eq!(parts, ["a", "\u{4E2D}", "e\u{301}"]);
    }

    #[test]
    fn display_width_sums_graphemes() {
        // "a" (1) + wide CJK (2) = 3.
        assert_eq!(display_width("a\u{4E2D}"), 3);
        assert_eq!(display_width(""), 0);
    }
}
