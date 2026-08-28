// The frontend's configuration, validated.
//
// PRD-mac §8 and §13. The values arrive from NSUserDefaults — which is why you
// can change the font with `defaults write` and no rebuild — but nothing
// arriving from outside is trusted: a font size of 0 or 10000 is a window that
// cannot be used, and it should be clamped rather than obeyed.
//
// The reading of NSUserDefaults is three lines in the view. The deciding is
// here, where it is tested.

#pragma once

#include <cstdint>
#include <string>
#include <vector>

namespace glue {

/// Font sizes outside this are not a preference, they are a mistake.
inline constexpr double kMinFontSize = 6.0;
inline constexpr double kMaxFontSize = 72.0;
inline constexpr double kDefaultFontSize = 13.0;

/// The window opens at the size every terminal has opened at since the VT100.
inline constexpr std::uint16_t kDefaultRows = 24;
inline constexpr std::uint16_t kDefaultCols = 80;

/// What NSUserDefaults returned, before anyone has decided whether to believe
/// it. Null and zero mean "not set", which is different from "set to nothing".
struct Defaults {
    const char* font_name = nullptr;
    double font_size = 0.0;
    const char* shell = nullptr;
    /// -1 for unset; 0 and 1 for the two answers.
    int option_is_meta = -1;
};

/// The settings the app actually runs with.
struct Config {
    /// Empty means the system's monospaced font, which is always present.
    std::string font_name;
    double font_size = kDefaultFontSize;
    std::string shell;
    std::vector<std::string> shell_args;
    bool option_is_meta = true;
    std::uint16_t rows = kDefaultRows;
    std::uint16_t cols = kDefaultCols;
};

/// Resolve the defaults into settings, filling in what was not set and
/// clamping what was set unreasonably.
///
/// `env_shell` is `$SHELL`, or null. A login shell is what rebuilds `PATH` when
/// the app was launched from Finder, so `-l` is not optional.
Config resolve(const Defaults& defaults, const char* env_shell);

/// A font size `steps` larger (or smaller), clamped. Cmd-+ and Cmd-- walk this.
double zoomed(double size, int steps);

}  // namespace glue
