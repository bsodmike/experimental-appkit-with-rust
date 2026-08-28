#include "Palette.h"

#include "terminal.h"

namespace glue {
namespace {

constexpr Rgba rgb(std::uint8_t r, std::uint8_t g, std::uint8_t b) { return Rgba{r, g, b, 255}; }

/// The 6x6x6 colour cube and the greyscale ramp are conventions, not something
/// to re-derive: every terminal uses these exact levels, and a program drawing
/// a gradient will look wrong against any others.
constexpr std::uint8_t kCubeLevels[6] = {0, 95, 135, 175, 215, 255};

Theme build_default_theme() {
    Theme theme;
    theme.background = rgb(0x1E, 0x1E, 0x1E);
    theme.foreground = rgb(0xD4, 0xD4, 0xD4);
    theme.cursor = rgb(0xD4, 0xD4, 0xD4);

    // 0-15: the ANSI colours, then their bright forms.
    const Rgba named[16] = {
        rgb(0x00, 0x00, 0x00), rgb(0xCD, 0x31, 0x31), rgb(0x0D, 0xBC, 0x79),
        rgb(0xE5, 0xE5, 0x10),           rgb(0x24, 0x72, 0xC8), rgb(0xBC, 0x3F, 0xBC),
        rgb(0x11, 0xA8, 0xCD),           rgb(0xE5, 0xE5, 0xE5), rgb(0x66, 0x66, 0x66),
        rgb(0xF1, 0x4C, 0x4C),           rgb(0x23, 0xD1, 0x8B), rgb(0xF5, 0xF5, 0x43),
        rgb(0x3B, 0x8E, 0xEA),           rgb(0xD6, 0x70, 0xD6), rgb(0x29, 0xB8, 0xDB),
        rgb(0xFF, 0xFF, 0xFF),
    };
    for (int i = 0; i < 16; ++i) {
        theme.palette[i] = named[i];
    }

    // 16-231: the cube, in the conventional r-major order.
    for (int i = 0; i < 216; ++i) {
        const int r = i / 36;
        const int g = (i / 6) % 6;
        const int b = i % 6;
        theme.palette[16 + i] = rgb(kCubeLevels[r], kCubeLevels[g], kCubeLevels[b]);
    }

    // 232-255: the greyscale ramp.
    for (int i = 0; i < 24; ++i) {
        const auto level = static_cast<std::uint8_t>(8 + (i * 10));
        theme.palette[232 + i] = rgb(level, level, level);
    }
    return theme;
}

Rgba blend(Rgba from, Rgba to, double amount) {
    const auto mix = [amount](std::uint8_t a, std::uint8_t b) {
        return static_cast<std::uint8_t>((a * (1.0 - amount)) + (b * amount) + 0.5);
    };
    return Rgba{mix(from.r, to.r), mix(from.g, to.g), mix(from.b, to.b), from.a};
}

bool has(std::uint16_t attrs, std::uint16_t flag) { return (attrs & flag) != 0; }

}  // namespace

const Theme& default_theme() {
    static const Theme theme = build_default_theme();
    return theme;
}

Rgba resolve_one(std::uint32_t packed, Rgba fallback, const Theme& theme) {
    switch (packed >> TERMINAL_COLOR_TAG_SHIFT) {
        case TERMINAL_COLOR_INDEXED:
            return theme.palette[packed & 0xFF];
        case TERMINAL_COLOR_RGB:
            return Rgba{static_cast<std::uint8_t>((packed >> 16) & 0xFF),
                        static_cast<std::uint8_t>((packed >> 8) & 0xFF),
                        static_cast<std::uint8_t>(packed & 0xFF), 255};
        case TERMINAL_COLOR_DEFAULT:
        default:
            // An unknown tag is treated as the default rather than as a colour
            // invented from its bits: a frontend guessing is worse than a
            // frontend deferring to the theme.
            return fallback;
    }
}

Resolved resolve(std::uint32_t packed_fg, std::uint32_t packed_bg, std::uint16_t attrs,
                 const Theme& theme) {
    Resolved out;
    out.fg = resolve_one(packed_fg, theme.foreground, theme);
    out.bg = resolve_one(packed_bg, theme.background, theme);

    // Bold brightens the ANSI eight, which is what every terminal does and what
    // colour schemes are designed against. It leaves indexed 8-255 and
    // truecolour alone, because those already said exactly what they wanted.
    if (has(attrs, TERMINAL_ATTR_BOLD) &&
        (packed_fg >> TERMINAL_COLOR_TAG_SHIFT) == TERMINAL_COLOR_INDEXED) {
        const std::uint32_t index = packed_fg & 0xFF;
        if (index < 8) {
            out.fg = theme.palette[index + 8];
        }
    }

    if (has(attrs, TERMINAL_ATTR_REVERSE)) {
        const Rgba swapped = out.fg;
        out.fg = out.bg;
        out.bg = swapped;
    }

    // Dim blends toward whatever the background ended up being, so it stays
    // legible after a reverse rather than dimming toward the wrong colour.
    if (has(attrs, TERMINAL_ATTR_DIM)) {
        out.fg = blend(out.fg, out.bg, 0.5);
    }

    // Hidden text is drawn in the background colour: still there, still
    // selectable and copyable once selection exists, simply not visible.
    if (has(attrs, TERMINAL_ATTR_HIDDEN)) {
        out.fg = out.bg;
    }

    return out;
}

}  // namespace glue
