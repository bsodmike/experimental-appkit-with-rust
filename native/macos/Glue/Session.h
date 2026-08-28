// The handle, owned by a destructor.
//
// PRD §6 gives the frontend exactly one obligation: call terminal_destroy once.
// A destructor is the only construct that cannot forget, which is the whole
// reason the bridge is Objective-C++ rather than Objective-C (PRD §13).

#pragma once

#include <cstdint>
#include <string>
#include <utility>
#include <vector>

#include "terminal.h"

namespace glue {

/// How to start the shell. Strings are owned here, so the caller does not have
/// to keep its own alive across the create call.
struct SessionConfig {
    std::string program = "/bin/zsh";
    std::vector<std::string> args = {"-l"};
    /// Empty means the user's home directory, which is what the engine does
    /// with it — an app bundle's own working directory is `/`.
    std::string cwd;
    std::vector<std::pair<std::string, std::string>> env;
    std::uint16_t rows = 24;
    std::uint16_t cols = 80;
    void (*wake_up)(void*) = nullptr;
    void* wake_up_ctx = nullptr;
};

/// Start logging the loop to `dir`, if `dir` is not empty. Safe to call more
/// than once; only the first call takes effect.
void init_logging(const std::string& dir);

/// A running terminal. Non-copyable, movable, and destroyed exactly once.
class Session {
  public:
    Session() = default;
    ~Session();

    Session(const Session&) = delete;
    Session& operator=(const Session&) = delete;
    Session(Session&& other) noexcept : handle_(std::exchange(other.handle_, nullptr)) {}
    Session& operator=(Session&& other) noexcept;

    /// Start a shell. `valid()` is false if it could not be started.
    static Session spawn(const SessionConfig& config);

    bool valid() const { return handle_ != nullptr; }
    TerminalSession* handle() const { return handle_; }

    TerminalStatus send_text(std::string_view text) const;
    TerminalStatus send_key(const TerminalKeyEvent& event) const;
    TerminalStatus paste(std::string_view text) const;
    TerminalStatus resize(std::uint16_t rows, std::uint16_t cols) const;

    /// The window title, copied out with the two-call sizing pattern (PRD §11).
    std::string title() const;

    bool has_hung_up() const;

    /// Stop early. The destructor does this anyway; calling it explicitly is how
    /// teardown gets ordered against the view that the wake-up points at
    /// (PRD-mac §9).
    void close();

  private:
    explicit Session(TerminalSession* handle) : handle_(handle) {}
    TerminalSession* handle_ = nullptr;
};

}  // namespace glue
