//! Keystrokes to bytes.
//!
//! PRD §5 puts this in Rust, which surprises people: `Ctrl+C` producing `0x03`,
//! and the arrow keys producing `CSI A` or `SS3 A` depending on DECCKM, are
//! properties of the *terminal*, not of macOS. The frontend reports which key
//! was pressed with which modifiers (PRD §8); what that becomes is decided here,
//! where the modes already live — and it is testable without touching a
//! keyboard.
//!
//! Committed text is a different channel and does not come through here: it
//! arrives from the input system as finished UTF-8 and passes straight through.
//! [`encode_paste`] is the exception, because a paste is text that the terminal
//! has to frame.

use crate::screen::Modes;

/// A key with terminal meaning, as the frontend reports it.
///
/// [`Key::Char`] carries the character the key would produce unmodified; it is
/// here for `Ctrl`/`Alt` combinations, which the text channel cannot express.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Key {
    Char(char),
    Enter,
    Tab,
    Backspace,
    Escape,
    Delete,
    Insert,
    Up,
    Down,
    Right,
    Left,
    Home,
    End,
    PageUp,
    PageDown,
    /// A function key, `F(1)` through `F(12)`.
    F(u8),
    /// A key on the numeric keypad, which sends different bytes in DECKPAM
    /// application mode.
    Keypad(Keypad),
}

/// The keypad keys that have an application-mode encoding of their own.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Keypad {
    Digit(u8),
    Enter,
    Plus,
    Minus,
    Multiply,
    Divide,
    Decimal,
    Equals,
}

/// The modifier keys held down with a keystroke.
///
/// `Cmd` is deliberately absent: on macOS it means an application command, and
/// PRD §8 says those never reach the engine at all.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug, Hash)]
pub struct Modifiers(u8);

impl Modifiers {
    pub const NONE: Self = Self(0);
    pub const SHIFT: Self = Self(1 << 0);
    pub const ALT: Self = Self(1 << 1);
    pub const CTRL: Self = Self(1 << 2);

    pub const fn bits(self) -> u8 {
        self.0
    }

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// The xterm modifier parameter: `1 + shift + 2*alt + 4*ctrl`, or `None`
    /// when nothing is held (in which case the parameter is left out entirely,
    /// because `CSI A` and `CSI 1;1A` are not the same sequence to every
    /// program).
    pub fn xterm_param(self) -> Option<u16> {
        if self.is_empty() {
            return None;
        }
        let mut n = 1;
        if self.contains(Self::SHIFT) {
            n += 1;
        }
        if self.contains(Self::ALT) {
            n += 2;
        }
        if self.contains(Self::CTRL) {
            n += 4;
        }
        Some(n)
    }
}

impl std::ops::BitOr for Modifiers {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

const ESC: u8 = 0x1B;

/// Encode one keystroke as the bytes to write to the PTY.
///
/// `modes` decides the two mode-dependent families: DECCKM for the cursor and
/// `Home`/`End` keys, DECKPAM for the keypad.
pub fn encode_key(key: Key, mods: Modifiers, modes: Modes) -> Vec<u8> {
    match key {
        Key::Char(c) => encode_char(c, mods),
        Key::Enter => with_alt(mods, b"\r".to_vec()),
        Key::Tab if mods.contains(Modifiers::SHIFT) => b"\x1b[Z".to_vec(),
        Key::Tab => with_alt(mods, b"\t".to_vec()),
        // Backspace sends DEL, as every terminfo entry since the VT220 expects;
        // Ctrl+Backspace is the one that sends the actual backspace byte.
        Key::Backspace if mods.contains(Modifiers::CTRL) => with_alt(mods, vec![0x08]),
        Key::Backspace => with_alt(mods, vec![0x7F]),
        Key::Escape => with_alt(mods, vec![ESC]),

        Key::Up => cursor_key(b'A', mods, modes),
        Key::Down => cursor_key(b'B', mods, modes),
        Key::Right => cursor_key(b'C', mods, modes),
        Key::Left => cursor_key(b'D', mods, modes),
        Key::Home => cursor_key(b'H', mods, modes),
        Key::End => cursor_key(b'F', mods, modes),

        Key::Insert => tilde_key(2, mods),
        Key::Delete => tilde_key(3, mods),
        Key::PageUp => tilde_key(5, mods),
        Key::PageDown => tilde_key(6, mods),

        // F1-F4 are the VT100 keypad-function keys and keep their SS3 form;
        // F5 upwards are the xterm tilde sequences, with a gap at 16 and 22
        // that is historical and must be reproduced.
        Key::F(n @ 1..=4) => {
            let final_byte = b'P' + (n - 1);
            match mods.xterm_param() {
                None => vec![ESC, b'O', final_byte],
                Some(m) => format!("\x1b[1;{m}{}", final_byte as char).into_bytes(),
            }
        }
        Key::F(n @ 5..=12) => {
            const CODES: [u16; 8] = [15, 17, 18, 19, 20, 21, 23, 24];
            tilde_key(CODES[(n - 5) as usize], mods)
        }
        Key::F(_) => Vec::new(), // beyond F12: nothing agreed on, so nothing sent

        Key::Keypad(k) => encode_keypad(k, mods, modes),
    }
}

/// Encode pasted text.
///
/// With bracketed paste on, the text is wrapped in `CSI 200~` / `CSI 201~` so
/// the program can tell a paste from typing — which is what stops a shell from
/// executing every line of a pasted script the moment it arrives.
///
/// The end marker is stripped from the text itself either way. Without that,
/// pasted content could close the bracket early and have its remainder treated
/// as typing, which is the whole attack bracketed paste exists to prevent.
/// Newlines become carriage returns, because that is what the Return key sends.
pub fn encode_paste(text: &str, modes: Modes) -> Vec<u8> {
    const END: &str = "\x1b[201~";
    let cleaned = text.replace(END, "").replace('\n', "\r");
    if !modes.bracketed_paste {
        return cleaned.into_bytes();
    }
    let mut out = Vec::with_capacity(cleaned.len() + 12);
    out.extend_from_slice(b"\x1b[200~");
    out.extend_from_slice(cleaned.as_bytes());
    out.extend_from_slice(END.as_bytes());
    out
}

/// The arrow and `Home`/`End` family: `CSI A` normally, `SS3 A` in application
/// cursor-key mode, and `CSI 1 ; m A` whenever a modifier is held — modified
/// keys use the CSI form even in application mode, as xterm does.
fn cursor_key(final_byte: u8, mods: Modifiers, modes: Modes) -> Vec<u8> {
    match mods.xterm_param() {
        Some(m) => format!("\x1b[1;{m}{}", final_byte as char).into_bytes(),
        None if modes.application_cursor_keys => vec![ESC, b'O', final_byte],
        None => vec![ESC, b'[', final_byte],
    }
}

/// The `CSI n ~` family (Insert, Delete, PageUp/Down, F5 and up).
fn tilde_key(code: u16, mods: Modifiers) -> Vec<u8> {
    match mods.xterm_param() {
        Some(m) => format!("\x1b[{code};{m}~").into_bytes(),
        None => format!("\x1b[{code}~").into_bytes(),
    }
}

fn encode_keypad(key: Keypad, mods: Modifiers, modes: Modes) -> Vec<u8> {
    if !modes.application_keypad {
        let c = match key {
            Keypad::Digit(d) => (b'0' + d.min(9)) as char,
            Keypad::Enter => return with_alt(mods, b"\r".to_vec()),
            Keypad::Plus => '+',
            Keypad::Minus => '-',
            Keypad::Multiply => '*',
            Keypad::Divide => '/',
            Keypad::Decimal => '.',
            Keypad::Equals => '=',
        };
        return encode_char(c, mods);
    }
    // DECKPAM: every keypad key gets its own SS3 sequence, which is how a
    // program can tell keypad 7 from the 7 above the letters.
    let final_byte = match key {
        Keypad::Digit(d) => b'p' + d.min(9),
        Keypad::Enter => b'M',
        Keypad::Plus => b'k',
        Keypad::Minus => b'm',
        Keypad::Multiply => b'j',
        Keypad::Divide => b'o',
        Keypad::Decimal => b'n',
        Keypad::Equals => b'X',
    };
    with_alt(mods, vec![ESC, b'O', final_byte])
}

/// A character key with modifiers. Unmodified characters normally arrive
/// through the text channel instead, but encoding them here costs nothing and
/// makes the function total.
fn encode_char(c: char, mods: Modifiers) -> Vec<u8> {
    let mut bytes = if mods.contains(Modifiers::CTRL) {
        match control_byte(c) {
            Some(b) => vec![b],
            // A control combination with no agreed byte (Ctrl+1, say) sends the
            // character unchanged, as xterm does.
            None => c.to_string().into_bytes(),
        }
    } else {
        c.to_string().into_bytes()
    };
    if mods.contains(Modifiers::ALT) {
        // Alt is "meta sends escape": the sequence is prefixed, not altered.
        bytes.insert(0, ESC);
    }
    bytes
}

/// The control byte a `Ctrl` combination produces, if it has one.
fn control_byte(c: char) -> Option<u8> {
    match c {
        'a'..='z' => Some(c as u8 & 0x1F),
        'A'..='Z' => Some(c.to_ascii_lowercase() as u8 & 0x1F),
        ' ' | '@' => Some(0x00),
        '[' => Some(0x1B),
        '\\' => Some(0x1C),
        ']' => Some(0x1D),
        '^' => Some(0x1E),
        '_' => Some(0x1F),
        '?' => Some(0x7F),
        _ => None,
    }
}

/// Prefix `ESC` when Alt is held, for the keys whose encoding has no modifier
/// parameter of its own.
fn with_alt(mods: Modifiers, mut bytes: Vec<u8>) -> Vec<u8> {
    if mods.contains(Modifiers::ALT) {
        bytes.insert(0, ESC);
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    const NONE: Modifiers = Modifiers::NONE;

    fn normal() -> Modes {
        Modes::default()
    }

    fn application() -> Modes {
        Modes {
            application_cursor_keys: true,
            application_keypad: true,
            ..Modes::default()
        }
    }

    fn encode(key: Key, mods: Modifiers, modes: Modes) -> String {
        String::from_utf8_lossy(&encode_key(key, mods, modes)).into_owned()
    }

    #[rstest]
    #[case(Key::Enter, "\r")]
    #[case(Key::Tab, "\t")]
    #[case(Key::Escape, "\x1b")]
    #[case(Key::Backspace, "\x7f")]
    #[case(Key::Delete, "\x1b[3~")]
    #[case(Key::Insert, "\x1b[2~")]
    #[case(Key::PageUp, "\x1b[5~")]
    #[case(Key::PageDown, "\x1b[6~")]
    fn the_plain_keys(#[case] key: Key, #[case] expected: &str) {
        assert_eq!(encode(key, NONE, normal()), expected);
    }

    #[rstest]
    #[case(Key::Up, "\x1b[A", "\x1bOA")]
    #[case(Key::Down, "\x1b[B", "\x1bOB")]
    #[case(Key::Right, "\x1b[C", "\x1bOC")]
    #[case(Key::Left, "\x1b[D", "\x1bOD")]
    #[case(Key::Home, "\x1b[H", "\x1bOH")]
    #[case(Key::End, "\x1b[F", "\x1bOF")]
    fn deccdkm_changes_the_cursor_keys(
        #[case] key: Key,
        #[case] normal_form: &str,
        #[case] application_form: &str,
    ) {
        assert_eq!(encode(key, NONE, normal()), normal_form);
        assert_eq!(encode(key, NONE, application()), application_form);
    }

    #[test]
    fn a_modified_cursor_key_uses_the_csi_form_in_either_mode() {
        // This is why the modifier parameter exists: `CSI 1;5A` is Ctrl+Up, and
        // no application-mode spelling of it exists.
        assert_eq!(encode(Key::Up, Modifiers::CTRL, normal()), "\x1b[1;5A");
        assert_eq!(encode(Key::Up, Modifiers::CTRL, application()), "\x1b[1;5A");
    }

    #[rstest]
    #[case(Modifiers::SHIFT, 2)]
    #[case(Modifiers::ALT, 3)]
    #[case(Modifiers::SHIFT | Modifiers::ALT, 4)]
    #[case(Modifiers::CTRL, 5)]
    #[case(Modifiers::SHIFT | Modifiers::CTRL, 6)]
    #[case(Modifiers::ALT | Modifiers::CTRL, 7)]
    #[case(Modifiers::SHIFT | Modifiers::ALT | Modifiers::CTRL, 8)]
    fn the_xterm_modifier_parameter(#[case] mods: Modifiers, #[case] expected: u16) {
        assert_eq!(mods.xterm_param(), Some(expected));
        assert_eq!(
            encode(Key::Up, mods, normal()),
            format!("\x1b[1;{expected}A")
        );
    }

    #[test]
    fn no_modifier_means_no_parameter() {
        // `CSI A` and `CSI 1;1A` are not the same sequence to every program.
        assert_eq!(NONE.xterm_param(), None);
        assert_eq!(encode(Key::Up, NONE, normal()), "\x1b[A");
    }

    #[rstest]
    #[case('c', "\x03")]
    #[case('C', "\x03")]
    #[case('a', "\x01")]
    #[case('z', "\x1a")]
    #[case('[', "\x1b")]
    #[case(' ', "\0")]
    #[case('?', "\x7f")]
    fn control_combinations(#[case] c: char, #[case] expected: &str) {
        assert_eq!(encode(Key::Char(c), Modifiers::CTRL, normal()), expected);
    }

    #[test]
    fn a_control_combination_with_no_byte_sends_the_character() {
        assert_eq!(encode(Key::Char('1'), Modifiers::CTRL, normal()), "1");
    }

    #[test]
    fn alt_prefixes_an_escape() {
        assert_eq!(encode(Key::Char('b'), Modifiers::ALT, normal()), "\x1bb");
        assert_eq!(
            encode(Key::Char('c'), Modifiers::ALT | Modifiers::CTRL, normal()),
            "\x1b\x03",
            "meta sends escape in front of the control byte, not instead of it"
        );
        assert_eq!(encode(Key::Enter, Modifiers::ALT, normal()), "\x1b\r");
    }

    #[test]
    fn shift_tab_is_a_sequence_of_its_own() {
        assert_eq!(encode(Key::Tab, Modifiers::SHIFT, normal()), "\x1b[Z");
    }

    #[test]
    fn backspace_sends_delete_unless_control_is_held() {
        assert_eq!(encode(Key::Backspace, NONE, normal()), "\x7f");
        assert_eq!(encode(Key::Backspace, Modifiers::CTRL, normal()), "\x08");
    }

    #[rstest]
    #[case(1, "\x1bOP")]
    #[case(4, "\x1bOS")]
    #[case(5, "\x1b[15~")]
    #[case(6, "\x1b[17~")]
    #[case(11, "\x1b[23~")]
    #[case(12, "\x1b[24~")]
    fn function_keys(#[case] n: u8, #[case] expected: &str) {
        assert_eq!(encode(Key::F(n), NONE, normal()), expected);
    }

    #[test]
    fn modified_function_keys_take_the_csi_form() {
        assert_eq!(encode(Key::F(1), Modifiers::SHIFT, normal()), "\x1b[1;2P");
        assert_eq!(encode(Key::F(5), Modifiers::SHIFT, normal()), "\x1b[15;2~");
    }

    #[test]
    fn a_function_key_we_have_no_encoding_for_sends_nothing() {
        assert!(encode_key(Key::F(13), NONE, normal()).is_empty());
    }

    #[rstest]
    #[case(Keypad::Digit(0), "0", "\x1bOp")]
    #[case(Keypad::Digit(9), "9", "\x1bOy")]
    #[case(Keypad::Enter, "\r", "\x1bOM")]
    #[case(Keypad::Plus, "+", "\x1bOk")]
    #[case(Keypad::Minus, "-", "\x1bOm")]
    #[case(Keypad::Multiply, "*", "\x1bOj")]
    #[case(Keypad::Divide, "/", "\x1bOo")]
    #[case(Keypad::Decimal, ".", "\x1bOn")]
    fn deckpam_changes_the_keypad(
        #[case] key: Keypad,
        #[case] normal_form: &str,
        #[case] application_form: &str,
    ) {
        assert_eq!(encode(Key::Keypad(key), NONE, normal()), normal_form);
        assert_eq!(
            encode(Key::Keypad(key), NONE, application()),
            application_form
        );
    }

    #[test]
    fn a_paste_is_bracketed_only_when_the_program_asked_for_it() {
        let mut modes = Modes::default();
        assert_eq!(encode_paste("ls", modes), b"ls");
        modes.bracketed_paste = true;
        assert_eq!(encode_paste("ls", modes), b"\x1b[200~ls\x1b[201~");
    }

    #[test]
    fn a_paste_cannot_close_its_own_bracket() {
        // Otherwise pasted content could end the bracket early and have its
        // remainder treated as typing -- the attack bracketing exists to stop.
        let modes = Modes {
            bracketed_paste: true,
            ..Modes::default()
        };
        let hostile = "safe\x1b[201~rm -rf /\r";
        let encoded = encode_paste(hostile, modes);
        let text = String::from_utf8_lossy(&encoded);
        assert_eq!(text, "\x1b[200~saferm -rf /\r\x1b[201~");
        assert_eq!(text.matches("\x1b[201~").count(), 1);
    }

    #[test]
    fn pasted_newlines_become_carriage_returns() {
        // A pasted line ending must look like the Return key, or a shell sees
        // nothing at all.
        let modes = Modes::default();
        assert_eq!(encode_paste("one\ntwo\n", modes), b"one\rtwo\r");
    }
}
