// The frontend's configuration, validated.
//
// PRD-mac §8. Everything the frontend can be configured by comes from one file
// in Ghostty's format (see ConfigFile.h); nothing comes from NSUserDefaults any
// more, so there is one place to look and one place to edit.
//
// Nothing arriving from that file is trusted. A font size of 0 or 10000 is a
// window that cannot be used, a palette index of 900 is a typo, and an unknown
// key is usually a misspelling of a real one. Each is reported against its line
// number and everything else in the file still applies -- a config that fails
// as a whole because of one bad line is a config that is painful to edit.

#pragma once

#include <cstdint>
#include <string>
#include <vector>

#include "ConfigFile.h"
#include "Palette.h"

namespace glue {

/// Font sizes outside this are not a preference, they are a mistake.
inline constexpr double kMinFontSize = 6.0;
inline constexpr double kMaxFontSize = 72.0;
inline constexpr double kDefaultFontSize = 13.0;

/// The window opens at the size every terminal has opened at since the VT100.
inline constexpr std::uint16_t kDefaultRows = 24;
inline constexpr std::uint16_t kDefaultCols = 80;

/// The settings the app actually runs with.
struct Config {
    /// Empty means the system's monospaced font, which is always present.
    std::string font_name;
    double font_size = kDefaultFontSize;
    std::string shell;
    std::vector<std::string> shell_args;
    bool option_is_meta = true;
    /// Where to write a log of the whole loop. Empty means no logging, which
    /// is the default: a terminal that writes a file every time you open it
    /// without being asked is a terminal nobody trusts.
    std::string log_dir;
    std::uint16_t rows = kDefaultRows;
    std::uint16_t cols = kDefaultCols;
    Theme theme;

    /// What was wrong with the file, in the order it was wrong. Shown to the
    /// user in every build: a typo is theirs to fix, and a setting that
    /// silently fails to apply is worse than a line of red text.
    std::vector<Diagnostic> diagnostics;
};

/// Turn parsed entries into settings.
///
/// `env_shell` is `$SHELL`, or null. Unset keys keep their defaults, and the
/// shell always runs as a login shell unless the file says otherwise: an app
/// bundle inherits a stub `PATH`, and only the login profile rebuilds it.
Config resolve(const ParsedFile& file, const char* env_shell);

/// A font size `steps` larger (or smaller), clamped. Cmd-+ and Cmd-- walk this.
double zoomed(double size, int steps);

}  // namespace glue
