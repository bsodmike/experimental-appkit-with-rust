#include "Session.h"

namespace glue {
namespace {

TerminalBytes bytes_of(const std::string& s) {
    return TerminalBytes{reinterpret_cast<const std::uint8_t*>(s.data()),
                         static_cast<std::uint32_t>(s.size())};
}

}  // namespace

Session::~Session() { close(); }

Session& Session::operator=(Session&& other) noexcept {
    if (this != &other) {
        close();
        handle_ = std::exchange(other.handle_, nullptr);
    }
    return *this;
}

void Session::close() {
    if (handle_ != nullptr) {
        // Joins the reader thread and hangs up the shell before returning
        // (PRD §7), so nothing is still running once this line is past.
        terminal_destroy(handle_);
        handle_ = nullptr;
    }
}

Session Session::spawn(const SessionConfig& config) {
    std::vector<TerminalBytes> args;
    args.reserve(config.args.size());
    for (const std::string& arg : config.args) {
        args.push_back(bytes_of(arg));
    }

    std::vector<TerminalEnvPair> env;
    env.reserve(config.env.size());
    for (const auto& pair : config.env) {
        env.push_back(TerminalEnvPair{bytes_of(pair.first), bytes_of(pair.second)});
    }

    TerminalConfig c{};
    c.size.rows = config.rows;
    c.size.cols = config.cols;
    c.program = bytes_of(config.program);
    c.args = args.data();
    c.args_len = static_cast<std::uint32_t>(args.size());
    c.cwd = bytes_of(config.cwd);
    c.env = env.data();
    c.env_len = static_cast<std::uint32_t>(env.size());
    c.wake_up = config.wake_up;
    c.wake_up_ctx = config.wake_up_ctx;

    // Every buffer above is borrowed only for the duration of this call
    // (PRD §6, rule 3), which is why the config owns its strings.
    return Session(terminal_create(&c));
}

TerminalStatus Session::send_text(std::string_view text) const {
    return terminal_send_text(handle_, reinterpret_cast<const std::uint8_t*>(text.data()),
                              static_cast<std::uint32_t>(text.size()));
}

TerminalStatus Session::send_key(const TerminalKeyEvent& event) const {
    return terminal_send_key(handle_, event);
}

TerminalStatus Session::paste(std::string_view text) const {
    return terminal_paste(handle_, reinterpret_cast<const std::uint8_t*>(text.data()),
                          static_cast<std::uint32_t>(text.size()));
}

TerminalStatus Session::resize(std::uint16_t rows, std::uint16_t cols) const {
    return terminal_resize(handle_, rows, cols);
}

std::string Session::title() const {
    std::uint32_t len = 0;
    if (terminal_copy_title(handle_, nullptr, 0, &len) == TerminalStatus_NullHandle) {
        return {};
    }
    if (len == 0) {
        return {};
    }
    std::string title(len, '\0');
    if (terminal_copy_title(handle_, reinterpret_cast<std::uint8_t*>(title.data()), len, &len) !=
        TerminalStatus_Ok) {
        return {};
    }
    return title;
}

bool Session::has_hung_up() const {
    bool gone = false;
    if (terminal_has_hung_up(handle_, &gone) != TerminalStatus_Ok) {
        // No handle means nothing left to show, which is the same answer.
        return true;
    }
    return gone;
}

}  // namespace glue
