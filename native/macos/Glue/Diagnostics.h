// What to show when the shell goes away.
//
// PRD-mac §13. Typing `exit` closes the window, as it does in Alacritty. A
// crash does not: the screen is kept, because a shell that died from an error
// printed that error immediately before dying, and taking it off screen is the
// least helpful moment to do so. Terminal.app and Ghostty both landed on some
// version of this rule.
//
// The rule is three comparisons, and it is here rather than in the view because
// getting it backwards means either losing an error message or never being able
// to close a window.

#pragma once

#include <string>
#include <string_view>

#include "terminal.h"

namespace glue {

/// What the frontend should do about the session having ended.
struct ExitPresentation {
    /// Close the window and quit: the session ended the way sessions end.
    bool close_window = false;
    /// Appended to the window title while the dead session is on screen.
    std::string title_suffix;
    /// Shown over the last frame. Empty in a Release build, and empty while the
    /// shell is still running.
    std::string overlay;
};

/// Decide what to do, given the child's status and whatever the engine last
/// complained about.
///
/// `debug_build` gates the overlay only: whether the window closes must not
/// depend on how the app was compiled.
ExitPresentation present_exit(const TerminalChildStatus& status, std::string_view last_error,
                              bool debug_build);

/// The name of a signal, for a human reading a title bar. "SIGTERM" beats "15".
std::string signal_name(int signal);

}  // namespace glue
