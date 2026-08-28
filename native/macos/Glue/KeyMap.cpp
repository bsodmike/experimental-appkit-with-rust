#include "KeyMap.h"

namespace glue {
namespace {

/// Carbon virtual key codes. They describe physical keys and have been stable
/// since before Mac OS X; AppKit reports them in NSEvent.keyCode.
enum : std::uint16_t {
    kVK_Return = 0x24,
    kVK_Tab = 0x30,
    kVK_Delete = 0x33,  // the key labelled Delete, which is Backspace
    kVK_Escape = 0x35,
    kVK_F5 = 0x60,
    kVK_F6 = 0x61,
    kVK_F7 = 0x62,
    kVK_F3 = 0x63,
    kVK_F8 = 0x64,
    kVK_F9 = 0x65,
    kVK_F11 = 0x67,
    kVK_F10 = 0x6D,
    kVK_F12 = 0x6F,
    kVK_Home = 0x73,
    kVK_PageUp = 0x74,
    kVK_ForwardDelete = 0x75,
    kVK_F4 = 0x76,
    kVK_End = 0x77,
    kVK_F2 = 0x78,
    kVK_PageDown = 0x79,
    kVK_F1 = 0x7A,
    kVK_LeftArrow = 0x7B,
    kVK_RightArrow = 0x7C,
    kVK_DownArrow = 0x7D,
    kVK_UpArrow = 0x7E,

    kVK_KeypadDecimal = 0x41,
    kVK_KeypadMultiply = 0x43,
    kVK_KeypadPlus = 0x45,
    kVK_KeypadDivide = 0x4B,
    kVK_KeypadEnter = 0x4C,
    kVK_KeypadMinus = 0x4E,
    kVK_KeypadEquals = 0x51,
    kVK_Keypad0 = 0x52,
    kVK_Keypad1 = 0x53,
    kVK_Keypad2 = 0x54,
    kVK_Keypad3 = 0x55,
    kVK_Keypad4 = 0x56,
    kVK_Keypad5 = 0x57,
    kVK_Keypad6 = 0x58,
    kVK_Keypad7 = 0x59,
    kVK_Keypad8 = 0x5B,
    kVK_Keypad9 = 0x5C,
};

Decision key(TerminalKeyCode code, std::uint16_t modifiers, std::uint8_t number = 0,
             std::uint32_t codepoint = 0) {
    Decision decision;
    decision.action = Action::SendKey;
    decision.event.code = code;
    decision.event.codepoint = codepoint;
    decision.event.number = number;
    decision.event.modifiers = modifiers;
    return decision;
}

bool special_key(std::uint16_t key_code, std::uint16_t modifiers, Decision& out) {
    switch (key_code) {
        case kVK_Return: out = key(TerminalKeyCode_Enter, modifiers); return true;
        case kVK_Tab: out = key(TerminalKeyCode_Tab, modifiers); return true;
        case kVK_Delete: out = key(TerminalKeyCode_Backspace, modifiers); return true;
        case kVK_Escape: out = key(TerminalKeyCode_Escape, modifiers); return true;
        case kVK_ForwardDelete: out = key(TerminalKeyCode_Delete, modifiers); return true;
        case kVK_Home: out = key(TerminalKeyCode_Home, modifiers); return true;
        case kVK_End: out = key(TerminalKeyCode_End, modifiers); return true;
        case kVK_PageUp: out = key(TerminalKeyCode_PageUp, modifiers); return true;
        case kVK_PageDown: out = key(TerminalKeyCode_PageDown, modifiers); return true;
        case kVK_UpArrow: out = key(TerminalKeyCode_Up, modifiers); return true;
        case kVK_DownArrow: out = key(TerminalKeyCode_Down, modifiers); return true;
        case kVK_LeftArrow: out = key(TerminalKeyCode_Left, modifiers); return true;
        case kVK_RightArrow: out = key(TerminalKeyCode_Right, modifiers); return true;

        case kVK_F1: out = key(TerminalKeyCode_F, modifiers, 1); return true;
        case kVK_F2: out = key(TerminalKeyCode_F, modifiers, 2); return true;
        case kVK_F3: out = key(TerminalKeyCode_F, modifiers, 3); return true;
        case kVK_F4: out = key(TerminalKeyCode_F, modifiers, 4); return true;
        case kVK_F5: out = key(TerminalKeyCode_F, modifiers, 5); return true;
        case kVK_F6: out = key(TerminalKeyCode_F, modifiers, 6); return true;
        case kVK_F7: out = key(TerminalKeyCode_F, modifiers, 7); return true;
        case kVK_F8: out = key(TerminalKeyCode_F, modifiers, 8); return true;
        case kVK_F9: out = key(TerminalKeyCode_F, modifiers, 9); return true;
        case kVK_F10: out = key(TerminalKeyCode_F, modifiers, 10); return true;
        case kVK_F11: out = key(TerminalKeyCode_F, modifiers, 11); return true;
        case kVK_F12: out = key(TerminalKeyCode_F, modifiers, 12); return true;

        case kVK_KeypadEnter: out = key(TerminalKeyCode_KeypadEnter, modifiers); return true;
        case kVK_KeypadPlus: out = key(TerminalKeyCode_KeypadPlus, modifiers); return true;
        case kVK_KeypadMinus: out = key(TerminalKeyCode_KeypadMinus, modifiers); return true;
        case kVK_KeypadMultiply: out = key(TerminalKeyCode_KeypadMultiply, modifiers); return true;
        case kVK_KeypadDivide: out = key(TerminalKeyCode_KeypadDivide, modifiers); return true;
        case kVK_KeypadDecimal: out = key(TerminalKeyCode_KeypadDecimal, modifiers); return true;
        case kVK_KeypadEquals: out = key(TerminalKeyCode_KeypadEquals, modifiers); return true;
        case kVK_Keypad0: out = key(TerminalKeyCode_KeypadDigit, modifiers, 0); return true;
        case kVK_Keypad1: out = key(TerminalKeyCode_KeypadDigit, modifiers, 1); return true;
        case kVK_Keypad2: out = key(TerminalKeyCode_KeypadDigit, modifiers, 2); return true;
        case kVK_Keypad3: out = key(TerminalKeyCode_KeypadDigit, modifiers, 3); return true;
        case kVK_Keypad4: out = key(TerminalKeyCode_KeypadDigit, modifiers, 4); return true;
        case kVK_Keypad5: out = key(TerminalKeyCode_KeypadDigit, modifiers, 5); return true;
        case kVK_Keypad6: out = key(TerminalKeyCode_KeypadDigit, modifiers, 6); return true;
        case kVK_Keypad7: out = key(TerminalKeyCode_KeypadDigit, modifiers, 7); return true;
        case kVK_Keypad8: out = key(TerminalKeyCode_KeypadDigit, modifiers, 8); return true;
        case kVK_Keypad9: out = key(TerminalKeyCode_KeypadDigit, modifiers, 9); return true;
        default: return false;
    }
}

}  // namespace

std::uint16_t modifiers_from_flags(std::uint32_t modifier_flags) {
    std::uint16_t modifiers = 0;
    if ((modifier_flags & kModShift) != 0) {
        modifiers |= TERMINAL_MOD_SHIFT;
    }
    if ((modifier_flags & kModOption) != 0) {
        modifiers |= TERMINAL_MOD_ALT;
    }
    if ((modifier_flags & kModControl) != 0) {
        modifiers |= TERMINAL_MOD_CTRL;
    }
    return modifiers;
}

std::uint32_t first_codepoint(const char* characters, std::size_t characters_len) {
    if (characters == nullptr || characters_len == 0) {
        return 0;
    }
    const auto* bytes = reinterpret_cast<const unsigned char*>(characters);
    const unsigned char lead = bytes[0];
    std::size_t extra = 0;
    std::uint32_t codepoint = 0;
    if (lead < 0x80) {
        return lead;
    }
    if ((lead & 0xE0) == 0xC0) {
        extra = 1;
        codepoint = lead & 0x1Fu;
    } else if ((lead & 0xF0) == 0xE0) {
        extra = 2;
        codepoint = lead & 0x0Fu;
    } else if ((lead & 0xF8) == 0xF0) {
        extra = 3;
        codepoint = lead & 0x07u;
    } else {
        return 0;  // a continuation byte or an invalid lead: not a character
    }
    if (characters_len < extra + 1) {
        return 0;
    }
    for (std::size_t i = 1; i <= extra; ++i) {
        if ((bytes[i] & 0xC0) != 0x80) {
            return 0;
        }
        codepoint = (codepoint << 6) | (bytes[i] & 0x3Fu);
    }
    return codepoint;
}

Decision map_key(std::uint16_t key_code, std::uint32_t modifier_flags, const char* characters,
                 std::size_t characters_len, Options options) {
    // Command is an application command and never reaches the engine (PRD §8).
    // Checked first, so that Cmd+C is not mistaken for Ctrl+C's neighbour.
    if ((modifier_flags & kModCommand) != 0) {
        Decision decision;
        decision.action = Action::Ignore;
        return decision;
    }

    const std::uint16_t modifiers = modifiers_from_flags(modifier_flags);

    Decision decision;
    if (special_key(key_code, modifiers, decision)) {
        return decision;
    }

    // A character key with Control, or with Option when Option is Meta, has to
    // be encoded by the engine: the layout's own answer for those is either the
    // wrong byte or no byte at all.
    const bool control = (modifier_flags & kModControl) != 0;
    const bool option_as_meta = options.option_is_meta && ((modifier_flags & kModOption) != 0);
    if (control || option_as_meta) {
        const std::uint32_t codepoint = first_codepoint(characters, characters_len);
        if (codepoint != 0) {
            return key(TerminalKeyCode_Char, modifiers, 0, codepoint);
        }
    }

    // Everything else is text, and the input system owns it — including dead
    // keys and IME composition, which cannot work any other way.
    decision.action = Action::SendAsText;
    return decision;
}

}  // namespace glue
