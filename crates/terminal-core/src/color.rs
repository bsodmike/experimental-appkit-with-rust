//! The terminal colour model.
//!
//! A cell's foreground and background are each one of three things: the
//! terminal *default* (whose concrete RGB the frontend chooses — the engine
//! never bakes in a theme), an index into the 256-colour palette, or a direct
//! truecolour RGB triple.
//!
//! Keeping `Default` distinct from any concrete colour matters: reverse-video,
//! selection highlighting and theme changes all need to know "this cell asked
//! for the default" rather than seeing a resolved RGB it can no longer tell
//! apart. Resolution to a concrete `u32` happens later, when runs are built for
//! the frontend (PRD §10); the engine reasons in these symbolic terms.

/// A foreground or background colour as the terminal understands it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
pub enum Color {
    /// The terminal's default foreground/background. The concrete RGB is the
    /// frontend's choice, not the engine's.
    #[default]
    Default,
    /// An index into the 256-colour palette. `0..=15` are the ANSI/bright
    /// named colours, `16..=231` the 6×6×6 colour cube, `232..=255` the
    /// grayscale ramp.
    Indexed(u8),
    /// A direct 24-bit truecolour value.
    Rgb(u8, u8, u8),
}

impl Color {
    // The sixteen named ANSI colours, as their conventional palette indices.
    // Provided as constructors so call sites read as intent ("red") rather than
    // as a magic number.
    pub const BLACK: Self = Self::Indexed(0);
    pub const RED: Self = Self::Indexed(1);
    pub const GREEN: Self = Self::Indexed(2);
    pub const YELLOW: Self = Self::Indexed(3);
    pub const BLUE: Self = Self::Indexed(4);
    pub const MAGENTA: Self = Self::Indexed(5);
    pub const CYAN: Self = Self::Indexed(6);
    pub const WHITE: Self = Self::Indexed(7);
    pub const BRIGHT_BLACK: Self = Self::Indexed(8);
    pub const BRIGHT_RED: Self = Self::Indexed(9);
    pub const BRIGHT_GREEN: Self = Self::Indexed(10);
    pub const BRIGHT_YELLOW: Self = Self::Indexed(11);
    pub const BRIGHT_BLUE: Self = Self::Indexed(12);
    pub const BRIGHT_MAGENTA: Self = Self::Indexed(13);
    pub const BRIGHT_CYAN: Self = Self::Indexed(14);
    pub const BRIGHT_WHITE: Self = Self::Indexed(15);

    /// Whether this colour defers to the frontend's theme rather than naming a
    /// concrete value.
    pub const fn is_default(self) -> bool {
        matches!(self, Color::Default)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_color_is_the_default_variant() {
        assert_eq!(Color::default(), Color::Default);
        assert!(Color::Default.is_default());
        assert!(!Color::RED.is_default());
        assert!(!Color::Rgb(0, 0, 0).is_default());
    }

    #[test]
    fn named_colors_map_to_their_palette_indices() {
        assert_eq!(Color::BLACK, Color::Indexed(0));
        assert_eq!(Color::WHITE, Color::Indexed(7));
        assert_eq!(Color::BRIGHT_BLACK, Color::Indexed(8));
        assert_eq!(Color::BRIGHT_WHITE, Color::Indexed(15));
    }

    #[test]
    fn default_is_distinct_from_any_concrete_color() {
        // The whole point of the Default variant: it never compares equal to a
        // resolved colour, so "asked for default" survives round-trips.
        assert_ne!(Color::Default, Color::Indexed(0));
        assert_ne!(Color::Default, Color::Rgb(0, 0, 0));
    }
}
