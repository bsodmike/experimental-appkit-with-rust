#include "Config.h"

#include <algorithm>
#include <cmath>
#include <cstdlib>

namespace glue {
namespace {

/// The shell to fall back to when nothing says otherwise. zsh has been the
/// macOS default since Catalina, and it exists on every supported system.
constexpr const char* kFallbackShell = "/bin/zsh";

bool empty_or_null(const char* s) { return s == nullptr || *s == '\0'; }

bool parse_bool(const std::string& value, bool& out) {
    if (value == "true" || value == "yes" || value == "1") {
        out = true;
        return true;
    }
    if (value == "false" || value == "no" || value == "0") {
        out = false;
        return true;
    }
    return false;
}

bool parse_double(const std::string& value, double& out) {
    try {
        std::size_t used = 0;
        const double parsed = std::stod(value, &used);
        if (used != value.size() || !std::isfinite(parsed)) {
            return false;
        }
        out = parsed;
        return true;
    } catch (...) {
        return false;
    }
}

bool parse_int(const std::string& value, long& out) {
    try {
        std::size_t used = 0;
        const long parsed = std::stol(value, &used);
        if (used != value.size()) {
            return false;
        }
        out = parsed;
        return true;
    } catch (...) {
        return false;
    }
}

/// The index in `palette-9`, or -1 when the key is not a palette key at all.
/// -2 when it is meant to be one but the index is not a number from 0 to 255,
/// which is worth telling the user about rather than calling the whole key
/// unknown.
constexpr const char* kPalettePrefix = "palette-";

int palette_index(const std::string& key) {
    const std::size_t prefix = std::char_traits<char>::length(kPalettePrefix);
    if (key.size() <= prefix || key.compare(0, prefix, kPalettePrefix) != 0) {
        return -1;
    }
    long parsed = 0;
    if (!parse_int(key.substr(prefix), parsed) || parsed < 0 || parsed > 255) {
        return -2;
    }
    return static_cast<int>(parsed);
}

}  // namespace

Config resolve(const ParsedFile& file, const char* env_shell) {
    Config config;
    config.theme = default_theme();
    config.diagnostics = file.diagnostics;

    const auto complain = [&config](const Entry& entry, const std::string& why) {
        config.diagnostics.push_back({entry.line, "'" + entry.key + "': " + why});
    };

    bool shell_args_given = false;

    for (const Entry& entry : file.entries) {
        if (entry.key == "font-family") {
            config.font_name = entry.value;
        } else if (entry.key == "font-size") {
            double size = 0;
            if (!parse_double(entry.value, size)) {
                complain(entry, "'" + entry.value + "' is not a number");
            } else if (size < kMinFontSize || size > kMaxFontSize) {
                complain(entry, "must be between 6 and 72");
                config.font_size = std::clamp(size, kMinFontSize, kMaxFontSize);
            } else {
                config.font_size = size;
            }
        } else if (entry.key == "background" || entry.key == "foreground" ||
                   entry.key == "cursor-color") {
            Rgba color{};
            if (!parse_color(entry.value, color)) {
                complain(entry, "'" + entry.value + "' is not a colour like #1e1e1e");
            } else if (entry.key == "background") {
                config.theme.background = color;
            } else if (entry.key == "foreground") {
                config.theme.foreground = color;
            } else {
                config.theme.cursor = color;
            }
        } else if (entry.key == "palette") {
            // Ghostty spells this `palette = 1=#ff6188`. This file does not, and
            // someone pasting from a Ghostty config deserves better than being
            // told the key is unknown.
            complain(entry, "colours are set one key at a time, as 'palette-1 = #ff6188'");
        } else if (palette_index(entry.key) == -2) {
            complain(entry, "expected a palette index from 0 to 255");
        } else if (const int index = palette_index(entry.key); index >= 0) {
            Rgba color{};
            if (!parse_color(entry.value, color)) {
                complain(entry, "'" + entry.value + "' is not a colour like #ff6188");
            } else {
                config.theme.palette[index] = color;
            }
        } else if (entry.key == "shell") {
            config.shell = entry.value;
        } else if (entry.key == "shell-arg") {
            if (!shell_args_given) {
                config.shell_args.clear();  // the file replaces the default, not appends to it
                shell_args_given = true;
            }
            config.shell_args.push_back(entry.value);
        } else if (entry.key == "option-is-meta") {
            bool flag = true;
            if (!parse_bool(entry.value, flag)) {
                complain(entry, "'" + entry.value + "' is not true or false");
            } else {
                config.option_is_meta = flag;
            }
        } else if (entry.key == "window-rows" || entry.key == "window-cols") {
            long value = 0;
            if (!parse_int(entry.value, value) || value < 1 || value > 1000) {
                complain(entry, "expected a number of cells between 1 and 1000");
            } else if (entry.key == "window-rows") {
                config.rows = static_cast<std::uint16_t>(value);
            } else {
                config.cols = static_cast<std::uint16_t>(value);
            }
        } else {
            complain(entry, "unknown setting");
        }
    }

    if (config.shell.empty()) {
        config.shell = empty_or_null(env_shell) ? kFallbackShell : env_shell;
    }
    if (!shell_args_given) {
        config.shell_args = {"-l"};
    }

    return config;
}

double zoomed(double size, int steps) {
    return std::clamp(size + steps, kMinFontSize, kMaxFontSize);
}

}  // namespace glue
