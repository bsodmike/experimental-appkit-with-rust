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

    /// Pack into the `u32` that crosses the FFI boundary in a render run
    /// (PRD §10).
    ///
    /// The top byte is a tag and the low bytes the payload, so `Default`
    /// survives the trip rather than being resolved to an RGB the frontend can
    /// no longer tell apart from a real colour:
    ///
    /// - `0x00_000000` — the terminal default; the frontend substitutes its theme
    /// - `0x01_0000II` — palette index `II`
    /// - `0x02_RRGGBB` — truecolour
    pub const fn pack(self) -> u32 {
        match self {
            Color::Default => 0,
            Color::Indexed(i) => (Self::TAG_INDEXED << 24) | i as u32,
            Color::Rgb(r, g, b) => {
                (Self::TAG_RGB << 24) | ((r as u32) << 16) | ((g as u32) << 8) | b as u32
            }
        }
    }

    /// The inverse of [`Color::pack`]. `None` for a tag or payload this
    /// encoding never produces.
    pub const fn unpack(bits: u32) -> Option<Self> {
        let payload = bits & 0x00ff_ffff;
        match bits >> 24 {
            Self::TAG_DEFAULT if payload == 0 => Some(Color::Default),
            Self::TAG_INDEXED if payload <= 0xff => Some(Color::Indexed(payload as u8)),
            Self::TAG_RGB => Some(Color::Rgb(
                (payload >> 16) as u8,
                (payload >> 8) as u8,
                payload as u8,
            )),
            _ => None,
        }
    }

    const TAG_DEFAULT: u32 = 0x00;
    const TAG_INDEXED: u32 = 0x01;
    const TAG_RGB: u32 = 0x02;
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

    #[test]
    fn packing_round_trips_every_variant() {
        for c in [
            Color::Default,
            Color::Indexed(0),
            Color::Indexed(255),
            Color::RED,
            Color::Rgb(0, 0, 0),
            Color::Rgb(255, 255, 255),
            Color::Rgb(1, 2, 3),
        ] {
            assert_eq!(Color::unpack(c.pack()), Some(c), "round trip of {c:?}");
        }
    }

    #[test]
    fn the_default_colour_packs_to_zero() {
        // A zeroed run therefore reads as "default on default", which is what a
        // frontend that forgets to fill a field should see.
        assert_eq!(Color::Default.pack(), 0);
    }

    #[test]
    fn packed_colours_stay_distinguishable_by_tag() {
        // Black-as-palette-0, black-as-RGB and the default must not collide, or
        // the frontend can no longer theme them apart.
        assert_ne!(Color::Indexed(0).pack(), Color::Default.pack());
        assert_ne!(Color::Indexed(0).pack(), Color::Rgb(0, 0, 0).pack());
        assert_eq!(Color::Indexed(9).pack(), 0x0100_0009);
        assert_eq!(Color::Rgb(0x11, 0x22, 0x33).pack(), 0x0211_2233);
    }

    #[test]
    fn unpacking_rejects_encodings_we_never_emit() {
        assert_eq!(Color::unpack(0x0300_0000), None, "unknown tag");
        assert_eq!(Color::unpack(0x0000_0001), None, "default with a payload");
        assert_eq!(Color::unpack(0x0101_0000), None, "index out of range");
    }
}
