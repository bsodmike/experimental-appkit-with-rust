// Colour resolution: packed engine colours to the RGBA that gets drawn.
//
// PRD-mac §8. The engine owns no theme (PRD §5), so it hands the frontend
// symbolic colours — "the default", "palette entry 4", "this exact RGB" — and
// resolving them is the frontend's job. The reverse, dim and hidden rules are
// the part worth testing: each one is a sentence in a standard that is easy to
// implement backwards.

#pragma once

#include <cstdint>

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

/// The one theme v0 has. A constant until it is a preference (PRD-mac §8).
const Theme& default_theme();

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
