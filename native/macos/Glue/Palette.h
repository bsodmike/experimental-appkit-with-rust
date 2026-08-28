// Colour resolution: packed engine colours to the RGBA that gets drawn.
//
// PRD-mac §8. The engine owns no theme (PRD §5), so it hands the frontend
// symbolic colours — "the default", "palette entry 4", "this exact RGB" — and
// resolving them is the frontend's job. The reverse, dim and hidden rules are
// the part worth testing: each one is a sentence in a standard that is easy to
// implement backwards.

#pragma once

#include <cstdint>
#include <string_view>

namespace glue {

struct Rgba {
    std::uint8_t r = 0;
    std::uint8_t g = 0;
    std::uint8_t b = 0;
    std::uint8_t a = 255;

    friend bool operator==(const Rgba& lhs, const Rgba& rhs) {
        return lhs.r == rhs.r && lhs.g == rhs.g && lhs.b == rhs.b && lhs.a == rhs.a;
    }
    friend bool operator!=(const Rgba& lhs, const Rgba& rhs) { return !(lhs == rhs); }
};

/// A theme: what "default" means, and the 256 palette entries.
struct Theme {
    Rgba foreground;
    Rgba background;
    Rgba cursor;
    Rgba palette[256];
};

/// The theme a config file starts from: what you get with no file at all.
const Theme& default_theme();

/// Parse `#rrggbb` or `#rgb`, case-insensitive. `false` and an untouched `out`
/// for anything else, including a missing `#` -- a colour that cannot be read
/// is reported to the user rather than guessed at.
bool parse_color(std::string_view text, Rgba& out);

/// The colours one run is actually drawn with.
struct Resolved {
    Rgba fg;
    Rgba bg;
};

/// Resolve a run's packed colours and attributes.
///
/// The order matters and is the order every terminal uses: brighten bold,
/// then swap for reverse, then blend for dim, then collapse for hidden. Doing
/// reverse before bold would brighten the background instead of the text.
Resolved resolve(std::uint32_t packed_fg, std::uint32_t packed_bg, std::uint16_t attrs,
                 const Theme& theme);

/// Unpack one colour on its own: the tag byte says whether the low bytes are a
/// palette index, an RGB triple, or nothing at all.
Rgba resolve_one(std::uint32_t packed, Rgba fallback, const Theme& theme);

}  // namespace glue
