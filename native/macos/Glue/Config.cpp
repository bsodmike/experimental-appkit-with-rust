#include "Config.h"

#include <algorithm>
#include <cmath>

namespace glue {
namespace {

/// The shell to fall back to when nothing says otherwise. zsh has been the
/// macOS default since Catalina, and it exists on every supported system.
constexpr const char* kFallbackShell = "/bin/zsh";

bool empty_or_null(const char* s) { return s == nullptr || *s == '\0'; }

}  // namespace

Config resolve(const Defaults& defaults, const char* env_shell) {
    Config config;

    if (!empty_or_null(defaults.font_name)) {
        config.font_name = defaults.font_name;
    }

    // A size of zero means "not set", not "invisible".
    config.font_size = defaults.font_size > 0.0
                           ? std::clamp(defaults.font_size, kMinFontSize, kMaxFontSize)
                           : kDefaultFontSize;

    if (!empty_or_null(defaults.shell)) {
        config.shell = defaults.shell;
    } else if (!empty_or_null(env_shell)) {
        config.shell = env_shell;
    } else {
        config.shell = kFallbackShell;
    }

    // Always a login shell: an app bundle inherits a stub PATH, and only the
    // login profile rebuilds it.
    config.shell_args = {"-l"};

    config.option_is_meta = defaults.option_is_meta < 0 ? true : defaults.option_is_meta != 0;

    return config;
}

double zoomed(double size, int steps) {
    return std::clamp(size + steps, kMinFontSize, kMaxFontSize);
}

}  // namespace glue
