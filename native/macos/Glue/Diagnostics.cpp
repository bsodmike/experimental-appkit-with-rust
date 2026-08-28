#include "Diagnostics.h"

namespace glue {

std::string signal_name(int signal) {
    switch (signal) {
        case 1: return "SIGHUP";
        case 2: return "SIGINT";
        case 3: return "SIGQUIT";
        case 4: return "SIGILL";
        case 6: return "SIGABRT";
        case 8: return "SIGFPE";
        case 9: return "SIGKILL";
        case 11: return "SIGSEGV";
        case 13: return "SIGPIPE";
        case 15: return "SIGTERM";
        default: return "signal " + std::to_string(signal);
    }
}

ExitPresentation present_exit(const TerminalChildStatus& status, std::string_view last_error,
                              bool debug_build) {
    ExitPresentation presentation;

    if (!status.hung_up) {
        return presentation;  // still running: nothing to present
    }

    if (status.exited && status.signal == 0 && status.exit_code == 0) {
        // The ordinary end of a session. Close, as Alacritty does.
        presentation.close_window = true;
        return presentation;
    }

    if (status.exited && status.signal != 0) {
        presentation.title_suffix = " [killed: " + signal_name(status.signal) + "]";
    } else if (status.exited) {
        presentation.title_suffix = " [exited: " + std::to_string(status.exit_code) + "]";
    } else {
        // Hung up without a status: the reader thread stopped for its own
        // reasons and nobody knows what became of the shell. Never treated as a
        // clean exit, because closing the window would hide the one case where
        // the engine itself is what went wrong.
        presentation.title_suffix = " [disconnected]";
    }

    if (debug_build) {
        presentation.overlay = presentation.title_suffix.empty()
                                   ? std::string("session ended")
                                   : presentation.title_suffix.substr(1);
        if (!status.exited) {
            presentation.overlay += "\nthe engine stopped reading; the shell's fate is unknown";
        }
        if (!last_error.empty()) {
            presentation.overlay += "\n";
            presentation.overlay += last_error;
        }
    }

    return presentation;
}

}  // namespace glue
