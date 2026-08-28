// Keystrokes: deciding what an NSEvent means before the engine encodes it.
//
// PRD-mac §6. AppKit reports a physical key and a set of modifiers; the engine
// turns a key with meaning into bytes. This decides which of those two things is
// happening, and it does so over plain integers so that it can be tested without
// a keyboard, a window server, or a Mac.

#pragma once

#include <cstddef>
#include <cstdint>

#include "terminal.h"

namespace glue {

/// The NSEvent modifier bits this cares about. AppKit sets others on some keys —
/// the arrows carry Function and NumericPad — so the mask matters: without it,
/// pressing Up would look like Up-with-modifiers and produce a different
/// sequence.
enum : std::uint32_t {
    kModShift = 1u << 17,
    kModControl = 1u << 18,
    kModOption = 1u << 19,
    kModCommand = 1u << 20,
    kModMask = kModShift | kModControl | kModOption | kModCommand,
};

/// What the view should do with a key press.
enum class Action {
    /// Send `event` through terminal_send_key.
    SendKey,
    /// Hand it to the input system and wait for insertText:. Ordinary typing,
    /// dead keys and IME composition all take this path.
    SendAsText,
    /// Do nothing: an application command, which never reaches the engine.
    Ignore,
};

struct Decision {
    Action action = Action::SendAsText;
    TerminalKeyEvent event{};
};

struct Options {
    /// Whether Option is Meta — whether Option+B sends ESC B rather than the "∫"
    /// the layout would produce. True is what a terminal is for; false is what
    /// someone typing Norwegian wants. A preference, once there are preferences.
    bool option_is_meta = true;
};

/// Decide what a key press means.
///
/// `characters` is the event's charactersIgnoringModifiers as UTF-8: the
/// character the key would produce on its own. Using the modified characters
/// here would ask for Ctrl+C and get back whatever control byte the layout had
/// already applied, which differs between layouts and is sometimes nothing.
Decision map_key(std::uint16_t key_code, std::uint32_t modifier_flags, const char* characters,
                 std::size_t characters_len, Options options = {});

/// Translate NSEvent modifier flags to the engine's bits.
std::uint16_t modifiers_from_flags(std::uint32_t modifier_flags);

/// Decode the first UTF-8 scalar of `characters`, or 0 if there is not one.
std::uint32_t first_codepoint(const char* characters, std::size_t characters_len);

}  // namespace glue
