//! A VT/ANSI parser built with nom, pure streaming.
//!
//! [`VtParser`] accumulates bytes across feeds (PRD §9: a PTY read can end
//! mid-sequence or mid-UTF-8) and emits typed [`Command`]s. It uses nom's
//! streaming combinators, so an incomplete sequence at the end of the buffer
//! yields `Err::Incomplete` and is kept until the next `feed`.
//!
//! The parser is deliberately pure: it produces commands and knows nothing about
//! `Screen`. A separate applier maps commands onto the engine.
//!
//! Coverage so far: printable UTF-8, the common C0 controls, CSI cursor moves
//! (CUU/CUD/CUF/CUB, CUP/HVP), erase (ED/EL) and SGR (attributes, 16/256/RGB
//! colour). Sequences that are recognised but not yet acted on are consumed and
//! reported as [`Command::Ignored`] (filtered out of `feed`'s output), so the
//! stream never stalls on them.

use compact_str::CompactString;
use nom::bytes::complete::take_till1;
use nom::bytes::streaming::{tag, take_while};
use nom::error::{Error, ErrorKind};
use nom::{Err, IResult, Needed};

use crate::color::Color;

const ESC: u8 = 0x1B;

/// A decoded terminal command.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Command {
    /// A run of printable text (one or more grapheme clusters' worth of bytes).
    Print(CompactString),
    Bell,
    Backspace,
    Tab,
    LineFeed,
    CarriageReturn,
    CursorUp(u16),
    CursorDown(u16),
    CursorForward(u16),
    CursorBack(u16),
    /// Absolute cursor move, converted to 0-based row/column.
    CursorPosition {
        row: u16,
        col: u16,
    },
    /// Absolute column move (CHA/HPA), 0-based.
    CursorColumn(u16),
    /// Absolute row move (VPA), 0-based, column unchanged.
    CursorLine(u16),
    /// Down one row, scrolling at the bottom margin (IND).
    Index,
    /// Up one row, scrolling at the top margin (RI).
    ReverseIndex,
    /// Index plus carriage return (NEL).
    NextLine,
    /// DECSTBM. `top` is 0-based; `bottom` is 0-based and inclusive, `None`
    /// meaning the last row. `CSI r` with no parameters resets to the full
    /// screen, which is `{ top: 0, bottom: None }`.
    SetScrollRegion {
        top: u16,
        bottom: Option<u16>,
    },
    /// Scroll the region up (SU) or down (SD), leaving the cursor where it is.
    ScrollUp(u16),
    ScrollDown(u16),
    /// Open (IL) or close (DL) blank rows at the cursor, within the region.
    InsertLines(u16),
    DeleteLines(u16),
    /// Open (ICH) or close (DCH) blank cells on the cursor's row.
    InsertChars(u16),
    DeleteChars(u16),
    /// Overwrite n cells from the cursor with blanks, moving nothing (ECH).
    EraseChars(u16),
    EraseInDisplay(EraseMode),
    EraseInLine(EraseMode),
    Sgr(Vec<Sgr>),
    /// DECSC/DECRC: stash or restore the cursor together with the pen.
    SaveCursor,
    RestoreCursor,
    /// Turn terminal modes on (`enabled`) or off. One sequence can carry
    /// several; unrecognised mode numbers are dropped rather than reported.
    SetModes {
        modes: Vec<Mode>,
        enabled: bool,
    },
    /// RIS: reset the terminal to its power-on state.
    Reset,
    /// DSR: the program is asking a question the engine must answer on the
    /// write side of the PTY. `5` is "are you there", `6` is "where is the
    /// cursor" (CPR).
    DeviceStatusReport(u16),
    /// DA: "what kind of terminal are you".
    DeviceAttributes,
    /// OSC 0/2: the window title the program wants shown.
    SetTitle(String),
    /// A recognised-but-unhandled sequence, already consumed. Never surfaced by
    /// [`VtParser::feed`]; kept as a variant so the parser can report progress.
    Ignored,
}

/// A terminal mode that can be turned on or off.
///
/// These are the modes the engine actually keeps: two that change what the
/// keyboard sends (and so are engine state, PRD §5), one that changes the write
/// path, and one the frontend reads to decide whether to draw a caret.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// DECCKM (`?1`): arrow keys send `SS3` rather than `CSI`.
    ApplicationCursorKeys,
    /// DECKPAM/DECKPNM (`ESC =` / `ESC >`): the keypad sends application codes.
    ApplicationKeypad,
    /// DECAWM (`?7`): text wraps at the right margin instead of overwriting the
    /// last column.
    AutoWrap,
    /// DECTCEM (`?25`): whether the cursor is drawn.
    CursorVisible,
    /// `?2004`: pasted text is bracketed with `CSI 200~` / `CSI 201~`.
    BracketedPaste,
    /// `?1049` (and the older `?47` / `?1047`): show the alternate screen.
    AlternateScreen,
}

/// The region an erase command clears, relative to the cursor.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EraseMode {
    /// From the cursor to the end (of line or display).
    ToEnd,
    /// From the start to the cursor.
    ToStart,
    /// The whole line or display.
    All,
}

/// One Select-Graphic-Rendition instruction.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Sgr {
    Reset,
    Bold,
    Dim,
    Italic,
    Underline,
    Reverse,
    Hidden,
    Strikethrough,
    NoBoldDim,
    NoItalic,
    NoUnderline,
    NoReverse,
    NoHidden,
    NoStrikethrough,
    Fg(Color),
    Bg(Color),
    DefaultFg,
    DefaultBg,
}

/// A stateful VT parser holding the bytes not yet consumed across feeds.
#[derive(Clone, Debug, Default)]
pub struct VtParser {
    buf: Vec<u8>,
}

impl VtParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed more bytes, returning the commands now decodable. Bytes forming an
    /// incomplete sequence at the end are retained for the next call.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Command> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        let mut pos = 0;
        loop {
            let input = &self.buf[pos..];
            if input.is_empty() {
                break;
            }
            match parse_command(input) {
                Ok((rest, cmd)) => {
                    let consumed = input.len() - rest.len();
                    // Guard against a zero-width success looping forever.
                    pos += consumed.max(1);
                    if cmd != Command::Ignored {
                        out.push(cmd);
                    }
                }
                Err(Err::Incomplete(_)) => break,
                Err(_) => pos += 1, // malformed: resync past one byte
            }
        }
        self.buf.drain(..pos);
        out
    }
}

fn error(input: &[u8]) -> Err<Error<&[u8]>> {
    Err::Error(Error::new(input, ErrorKind::Tag))
}

fn parse_command(input: &[u8]) -> IResult<&[u8], Command> {
    // Ordered so ESC is handled before the generic C0 branch, and printable text
    // last. Each branch keys off the first byte, so there is no ambiguity.
    match input.first() {
        Some(&ESC) => parse_esc(input),
        Some(&b) if b < 0x20 || b == 0x7F => parse_c0(input),
        Some(_) => parse_print(input),
        None => Err(Err::Incomplete(Needed::new(1))),
    }
}

fn parse_print(input: &[u8]) -> IResult<&[u8], Command> {
    // Complete-mode: emit the printable run we have rather than waiting for a
    // terminator (text should display as it arrives).
    let (rest, taken) = take_till1(|b: u8| b < 0x20 || b == 0x7F)(input)?;
    if !rest.is_empty() {
        // A control byte terminates the run: decode lossily and take it all.
        let text = String::from_utf8_lossy(taken);
        return Ok((rest, Command::Print(CompactString::from(text.as_ref()))));
    }
    // The run reaches the end of the buffer: keep any trailing partial UTF-8.
    match std::str::from_utf8(taken) {
        Ok(s) => Ok((rest, Command::Print(CompactString::from(s)))),
        Err(e) => {
            let valid = e.valid_up_to();
            if valid > 0 {
                let s = std::str::from_utf8(&taken[..valid]).unwrap();
                Ok((&input[valid..], Command::Print(CompactString::from(s))))
            } else if e.error_len().is_none() {
                // Incomplete multi-byte character: wait for more bytes.
                Err(Err::Incomplete(Needed::new(1)))
            } else {
                // Genuinely invalid leading byte: emit a replacement, consume one.
                Ok((&input[1..], Command::Print(CompactString::from("\u{FFFD}"))))
            }
        }
    }
}

fn parse_c0(input: &[u8]) -> IResult<&[u8], Command> {
    let cmd = match input[0] {
        0x07 => Command::Bell,
        0x08 => Command::Backspace,
        0x09 => Command::Tab,
        0x0A..=0x0C => Command::LineFeed,
        0x0D => Command::CarriageReturn,
        _ => Command::Ignored,
    };
    Ok((&input[1..], cmd))
}

fn parse_esc(input: &[u8]) -> IResult<&[u8], Command> {
    let (rest, _) = tag(&[ESC][..])(input)?;
    match rest.first() {
        None => Err(Err::Incomplete(Needed::new(1))),
        Some(b'[') => parse_csi(&rest[1..]),
        Some(b']') => parse_osc(&rest[1..]),
        // The single-byte C1 escapes we act on. Anything else falls through to
        // the generic consumer below.
        Some(b'D') => Ok((&rest[1..], Command::Index)),
        Some(b'M') => Ok((&rest[1..], Command::ReverseIndex)),
        Some(b'E') => Ok((&rest[1..], Command::NextLine)),
        Some(b'7') => Ok((&rest[1..], Command::SaveCursor)),
        Some(b'8') => Ok((&rest[1..], Command::RestoreCursor)),
        Some(b'c') => Ok((&rest[1..], Command::Reset)),
        Some(b'=') => Ok((&rest[1..], keypad_mode(true))),
        Some(b'>') => Ok((&rest[1..], keypad_mode(false))),
        Some(_) => parse_esc_other(rest),
    }
}

/// An escape sequence we do not yet act on: optional intermediates then a final
/// byte. Consumed whole so the stream advances cleanly.
fn parse_esc_other(input: &[u8]) -> IResult<&[u8], Command> {
    let (rest, _) = take_while(|b| (0x20..=0x2F).contains(&b))(input)?;
    match rest.first() {
        None => Err(Err::Incomplete(Needed::new(1))),
        Some(&f) if (0x30..=0x7E).contains(&f) => Ok((&rest[1..], Command::Ignored)),
        Some(_) => Err(error(input)),
    }
}

/// CSI body (bytes after `ESC [`): parameter bytes, intermediates, final byte.
fn parse_csi(input: &[u8]) -> IResult<&[u8], Command> {
    let (rest, params) = take_while(|b| (0x30..=0x3F).contains(&b))(input)?;
    let (rest, _inter) = take_while(|b| (0x20..=0x2F).contains(&b))(rest)?;
    match rest.first() {
        None => Err(Err::Incomplete(Needed::new(1))),
        Some(&final_b) if (0x40..=0x7E).contains(&final_b) => {
            Ok((&rest[1..], dispatch_csi(params, final_b)))
        }
        Some(_) => Err(error(input)),
    }
}

/// OSC body (bytes after `ESC ]`): consume up to the terminator (`BEL`, or
/// `ST` = `ESC \`), then interpret it.
fn parse_osc(input: &[u8]) -> IResult<&[u8], Command> {
    let mut i = 0;
    while i < input.len() {
        match input[i] {
            0x07 => return Ok((&input[i + 1..], osc_command(&input[..i]))),
            ESC if i + 1 < input.len() && input[i + 1] == b'\\' => {
                return Ok((&input[i + 2..], osc_command(&input[..i])));
            }
            ESC if i + 1 >= input.len() => break, // maybe start of ST; need more
            _ => i += 1,
        }
    }
    Err(Err::Incomplete(Needed::new(1)))
}

/// Interpret an OSC body (`Ps ; Pt`). Only the title-setting commands are acted
/// on; the rest — colour queries, hyperlinks, the working directory — are
/// consumed and dropped.
fn osc_command(body: &[u8]) -> Command {
    let mut parts = body.splitn(2, |&b| b == b';');
    let ps = parts.next().unwrap_or(b"");
    let Some(text) = parts.next() else {
        return Command::Ignored;
    };
    // 0 sets both the window title and the icon name, 2 sets the title. 1 sets
    // the icon name alone, which has no place in this UI, so it is dropped.
    if ps != b"0" && ps != b"2" {
        return Command::Ignored;
    }
    // Titles come from the program and are not trusted to be UTF-8; control
    // characters are stripped so a title can never redraw anything.
    let text = String::from_utf8_lossy(text);
    Command::SetTitle(text.chars().filter(|c| !c.is_control()).collect())
}

fn dispatch_csi(params_bytes: &[u8], final_b: u8) -> Command {
    // A private-parameter prefix (`?`, `>`, `=`, `<`) makes the final byte mean
    // something else entirely -- `CSI ? 25 h` is not `CSI 25 h` -- so those
    // sequences never reach the standard dispatch below.
    if let Some(&prefix) = params_bytes.first()
        && (0x3C..=0x3F).contains(&prefix)
    {
        return dispatch_csi_private(prefix, &params_bytes[1..], final_b);
    }
    let params = parse_params(params_bytes);
    match final_b {
        b'A' => Command::CursorUp(nonzero(param(&params, 0, 1))),
        b'B' => Command::CursorDown(nonzero(param(&params, 0, 1))),
        b'C' => Command::CursorForward(nonzero(param(&params, 0, 1))),
        b'D' => Command::CursorBack(nonzero(param(&params, 0, 1))),
        b'H' | b'f' => Command::CursorPosition {
            row: nonzero(param(&params, 0, 1)) - 1,
            col: nonzero(param(&params, 1, 1)) - 1,
        },
        b'G' | b'`' => Command::CursorColumn(nonzero(param(&params, 0, 1)) - 1),
        b'd' => Command::CursorLine(nonzero(param(&params, 0, 1)) - 1),
        b'S' => Command::ScrollUp(nonzero(param(&params, 0, 1))),
        b'T' => Command::ScrollDown(nonzero(param(&params, 0, 1))),
        b'L' => Command::InsertLines(nonzero(param(&params, 0, 1))),
        b'M' => Command::DeleteLines(nonzero(param(&params, 0, 1))),
        b'@' => Command::InsertChars(nonzero(param(&params, 0, 1))),
        b'P' => Command::DeleteChars(nonzero(param(&params, 0, 1))),
        b'X' => Command::EraseChars(nonzero(param(&params, 0, 1))),
        b'r' => Command::SetScrollRegion {
            top: nonzero(param(&params, 0, 1)) - 1,
            // An omitted or zero bottom means "the last row", which only the
            // screen knows; the parser does not invent a height.
            bottom: match params.get(1).copied().flatten() {
                Some(0) | None => None,
                Some(n) => Some(n - 1),
            },
        },
        // SCOSC/SCORC, the ANSI spelling of DECSC/DECRC.
        b's' => Command::SaveCursor,
        b'u' => Command::RestoreCursor,
        b'J' => Command::EraseInDisplay(erase_mode(param(&params, 0, 0))),
        b'K' => Command::EraseInLine(erase_mode(param(&params, 0, 0))),
        b'm' => Command::Sgr(parse_sgr(&params)),
        b'n' => Command::DeviceStatusReport(param(&params, 0, 0)),
        b'c' => Command::DeviceAttributes,
        _ => Command::Ignored,
    }
}

/// CSI sequences carrying a private-parameter prefix.
///
/// Only DEC private mode set/reset (`CSI ? Pm h` / `l`) is acted on; everything
/// else is consumed so the stream advances. The alternate screen (`?1049` and
/// friends) lands here in a later slice.
fn dispatch_csi_private(prefix: u8, params_bytes: &[u8], final_b: u8) -> Command {
    if prefix != b'?' || !matches!(final_b, b'h' | b'l') {
        return Command::Ignored;
    }
    let modes: Vec<Mode> = parse_params(params_bytes)
        .into_iter()
        .flatten()
        .filter_map(private_mode)
        .collect();
    if modes.is_empty() {
        return Command::Ignored;
    }
    Command::SetModes {
        modes,
        enabled: final_b == b'h',
    }
}

fn private_mode(n: u16) -> Option<Mode> {
    match n {
        1 => Some(Mode::ApplicationCursorKeys),
        7 => Some(Mode::AutoWrap),
        25 => Some(Mode::CursorVisible),
        // 1049 also saves and restores the cursor, which falls out of setting
        // the whole primary buffer aside and swapping it back.
        47 | 1047 | 1049 => Some(Mode::AlternateScreen),
        2004 => Some(Mode::BracketedPaste),
        _ => None,
    }
}

fn keypad_mode(enabled: bool) -> Command {
    Command::SetModes {
        modes: vec![Mode::ApplicationKeypad],
        enabled,
    }
}

/// Split `;`-separated decimal parameters; an empty field is `None` (use the
/// default). A field with any non-digit (e.g. a private `?` marker) is `None`.
fn parse_params(bytes: &[u8]) -> Vec<Option<u16>> {
    if bytes.is_empty() {
        return Vec::new();
    }
    bytes
        .split(|&b| b == b';')
        .map(|seg| {
            if seg.is_empty() || !seg.iter().all(u8::is_ascii_digit) {
                return None;
            }
            let mut n: u32 = 0;
            for &d in seg {
                n = n.saturating_mul(10).saturating_add((d - b'0') as u32);
            }
            Some(n.min(u16::MAX as u32) as u16)
        })
        .collect()
}

fn param(params: &[Option<u16>], idx: usize, default: u16) -> u16 {
    params.get(idx).copied().flatten().unwrap_or(default)
}

/// Cursor-movement counts treat 0 as 1.
fn nonzero(n: u16) -> u16 {
    if n == 0 { 1 } else { n }
}

fn erase_mode(n: u16) -> EraseMode {
    match n {
        1 => EraseMode::ToStart,
        2 | 3 => EraseMode::All,
        _ => EraseMode::ToEnd,
    }
}

fn parse_sgr(params: &[Option<u16>]) -> Vec<Sgr> {
    if params.is_empty() {
        return vec![Sgr::Reset]; // bare `CSI m` == `CSI 0 m`
    }
    let mut out = Vec::new();
    let mut i = 0;
    while i < params.len() {
        let code = params[i].unwrap_or(0);
        match code {
            0 => out.push(Sgr::Reset),
            1 => out.push(Sgr::Bold),
            2 => out.push(Sgr::Dim),
            3 => out.push(Sgr::Italic),
            4 => out.push(Sgr::Underline),
            7 => out.push(Sgr::Reverse),
            8 => out.push(Sgr::Hidden),
            9 => out.push(Sgr::Strikethrough),
            22 => out.push(Sgr::NoBoldDim),
            23 => out.push(Sgr::NoItalic),
            24 => out.push(Sgr::NoUnderline),
            27 => out.push(Sgr::NoReverse),
            28 => out.push(Sgr::NoHidden),
            29 => out.push(Sgr::NoStrikethrough),
            30..=37 => out.push(Sgr::Fg(Color::Indexed((code - 30) as u8))),
            39 => out.push(Sgr::DefaultFg),
            40..=47 => out.push(Sgr::Bg(Color::Indexed((code - 40) as u8))),
            49 => out.push(Sgr::DefaultBg),
            90..=97 => out.push(Sgr::Fg(Color::Indexed((code - 90 + 8) as u8))),
            100..=107 => out.push(Sgr::Bg(Color::Indexed((code - 100 + 8) as u8))),
            38 => {
                if let Some((c, advance)) = parse_ext_color(params, i + 1) {
                    out.push(Sgr::Fg(c));
                    i += advance;
                }
            }
            48 => {
                if let Some((c, advance)) = parse_ext_color(params, i + 1) {
                    out.push(Sgr::Bg(c));
                    i += advance;
                }
            }
            _ => {} // unknown attribute: ignore
        }
        i += 1;
    }
    out
}

/// Decode the extended-colour tail after a `38`/`48`: `5;n` (indexed) or
/// `2;r;g;b` (truecolour). Returns the colour and how many parameters after the
/// `38`/`48` it consumed.
fn parse_ext_color(params: &[Option<u16>], start: usize) -> Option<(Color, usize)> {
    match params.get(start).copied().flatten() {
        Some(5) => {
            let n = params.get(start + 1).copied().flatten()? as u8;
            Some((Color::Indexed(n), 2))
        }
        Some(2) => {
            let r = params.get(start + 1).copied().flatten()? as u8;
            let g = params.get(start + 2).copied().flatten()? as u8;
            let b = params.get(start + 3).copied().flatten()? as u8;
            Some((Color::Rgb(r, g, b), 4))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    fn feed(bytes: &[u8]) -> Vec<Command> {
        VtParser::new().feed(bytes)
    }

    #[rstest]
    #[case(b"\x07", Command::Bell)]
    #[case(b"\x08", Command::Backspace)]
    #[case(b"\x09", Command::Tab)]
    #[case(b"\x0a", Command::LineFeed)]
    #[case(b"\x0d", Command::CarriageReturn)]
    fn c0_controls(#[case] input: &[u8], #[case] expected: Command) {
        assert_eq!(feed(input), vec![expected]);
    }

    #[test]
    fn printable_text_becomes_one_print() {
        assert_eq!(feed(b"hello"), vec![Command::Print("hello".into())]);
    }

    #[test]
    fn text_and_control_interleave() {
        assert_eq!(
            feed(b"ab\r\ncd"),
            vec![
                Command::Print("ab".into()),
                Command::CarriageReturn,
                Command::LineFeed,
                Command::Print("cd".into()),
            ]
        );
    }

    #[rstest]
    #[case(b"\x1b[A", Command::CursorUp(1))]
    #[case(b"\x1b[3A", Command::CursorUp(3))]
    #[case(b"\x1b[0A", Command::CursorUp(1))] // 0 means 1
    #[case(b"\x1b[B", Command::CursorDown(1))]
    #[case(b"\x1b[2C", Command::CursorForward(2))]
    #[case(b"\x1b[D", Command::CursorBack(1))]
    fn csi_cursor_moves(#[case] input: &[u8], #[case] expected: Command) {
        assert_eq!(feed(input), vec![expected]);
    }

    #[rstest]
    #[case(b"\x1b[H", Command::CursorPosition { row: 0, col: 0 })]
    #[case(b"\x1b[5;9H", Command::CursorPosition { row: 4, col: 8 })]
    #[case(b"\x1b[5;9f", Command::CursorPosition { row: 4, col: 8 })]
    fn csi_cursor_position(#[case] input: &[u8], #[case] expected: Command) {
        assert_eq!(feed(input), vec![expected]);
    }

    #[rstest]
    #[case(b"\x1b[G", Command::CursorColumn(0))]
    #[case(b"\x1b[9G", Command::CursorColumn(8))]
    #[case(b"\x1b[9`", Command::CursorColumn(8))]
    #[case(b"\x1b[4d", Command::CursorLine(3))]
    #[case(b"\x1bD", Command::Index)]
    #[case(b"\x1bM", Command::ReverseIndex)]
    #[case(b"\x1bE", Command::NextLine)]
    #[case(b"\x1b[S", Command::ScrollUp(1))]
    #[case(b"\x1b[3S", Command::ScrollUp(3))]
    #[case(b"\x1b[2T", Command::ScrollDown(2))]
    #[case(b"\x1b[2L", Command::InsertLines(2))]
    #[case(b"\x1b[M", Command::DeleteLines(1))]
    #[case(b"\x1b[3@", Command::InsertChars(3))]
    #[case(b"\x1b[3P", Command::DeleteChars(3))]
    #[case(b"\x1b[3X", Command::EraseChars(3))]
    fn csi_editing_and_index(#[case] input: &[u8], #[case] expected: Command) {
        assert_eq!(feed(input), vec![expected]);
    }

    #[rstest]
    #[case(b"\x1b[r", Command::SetScrollRegion { top: 0, bottom: None })]
    #[case(b"\x1b[3;10r", Command::SetScrollRegion { top: 2, bottom: Some(9) })]
    #[case(b"\x1b[5r", Command::SetScrollRegion { top: 4, bottom: None })]
    #[case(b"\x1b[;10r", Command::SetScrollRegion { top: 0, bottom: Some(9) })]
    fn csi_scroll_region(#[case] input: &[u8], #[case] expected: Command) {
        assert_eq!(feed(input), vec![expected]);
    }

    #[rstest]
    #[case(b"\x1b7", Command::SaveCursor)]
    #[case(b"\x1b8", Command::RestoreCursor)]
    #[case(b"\x1b[s", Command::SaveCursor)]
    #[case(b"\x1b[u", Command::RestoreCursor)]
    #[case(b"\x1bc", Command::Reset)]
    fn save_restore_and_reset(#[case] input: &[u8], #[case] expected: Command) {
        assert_eq!(feed(input), vec![expected]);
    }

    #[rstest]
    #[case(b"\x1b[?1h", vec![Mode::ApplicationCursorKeys], true)]
    #[case(b"\x1b[?1l", vec![Mode::ApplicationCursorKeys], false)]
    #[case(b"\x1b[?7l", vec![Mode::AutoWrap], false)]
    #[case(b"\x1b[?25h", vec![Mode::CursorVisible], true)]
    #[case(b"\x1b[?2004h", vec![Mode::BracketedPaste], true)]
    #[case(b"\x1b[?1;25h", vec![Mode::ApplicationCursorKeys, Mode::CursorVisible], true)]
    #[case(b"\x1b=", vec![Mode::ApplicationKeypad], true)]
    #[case(b"\x1b>", vec![Mode::ApplicationKeypad], false)]
    fn mode_set_and_reset(#[case] input: &[u8], #[case] modes: Vec<Mode>, #[case] enabled: bool) {
        assert_eq!(feed(input), vec![Command::SetModes { modes, enabled }]);
    }

    #[rstest]
    #[case(b"\x1b[?1049h", true)]
    #[case(b"\x1b[?1047h", true)]
    #[case(b"\x1b[?47h", true)]
    #[case(b"\x1b[?1049l", false)]
    fn the_alternate_screen_has_three_spellings(#[case] input: &[u8], #[case] enabled: bool) {
        assert_eq!(
            feed(input),
            vec![Command::SetModes {
                modes: vec![Mode::AlternateScreen],
                enabled,
            }]
        );
    }

    #[test]
    fn unknown_modes_are_dropped_and_known_ones_kept() {
        // `?1048` (save the cursor only) is not handled, so it vanishes rather
        // than being reported as something the engine acted on.
        assert_eq!(feed(b"\x1b[?1048h"), vec![]);
        assert_eq!(
            feed(b"\x1b[?1048;25h"),
            vec![Command::SetModes {
                modes: vec![Mode::CursorVisible],
                enabled: true,
            }]
        );
    }

    #[rstest]
    #[case(b"\x1b[5n", Command::DeviceStatusReport(5))]
    #[case(b"\x1b[6n", Command::DeviceStatusReport(6))]
    #[case(b"\x1b[c", Command::DeviceAttributes)]
    #[case(b"\x1b[0c", Command::DeviceAttributes)]
    fn queries(#[case] input: &[u8], #[case] expected: Command) {
        assert_eq!(feed(input), vec![expected]);
    }

    #[test]
    fn a_private_prefix_never_reaches_the_standard_dispatch() {
        // `CSI ? 3 r` is XTRESTORE, not a scroll region, and `CSI ? 1 J` is
        // DECSED, not an erase. Both are consumed without acting.
        assert_eq!(feed(b"\x1b[?3r"), vec![]);
        assert_eq!(feed(b"\x1b[?1J"), vec![]);
        assert_eq!(feed(b"\x1b[>c"), vec![]);
        assert_eq!(feed(b"\x1b[?7s"), vec![], "XTSAVE, not a cursor save");
    }

    #[rstest]
    #[case(b"\x1b[J", Command::EraseInDisplay(EraseMode::ToEnd))]
    #[case(b"\x1b[1J", Command::EraseInDisplay(EraseMode::ToStart))]
    #[case(b"\x1b[2J", Command::EraseInDisplay(EraseMode::All))]
    #[case(b"\x1b[K", Command::EraseInLine(EraseMode::ToEnd))]
    #[case(b"\x1b[1K", Command::EraseInLine(EraseMode::ToStart))]
    fn csi_erase(#[case] input: &[u8], #[case] expected: Command) {
        assert_eq!(feed(input), vec![expected]);
    }

    #[rstest]
    #[case(b"\x1b[m", vec![Sgr::Reset])]
    #[case(b"\x1b[0m", vec![Sgr::Reset])]
    #[case(b"\x1b[1m", vec![Sgr::Bold])]
    #[case(b"\x1b[1;4m", vec![Sgr::Bold, Sgr::Underline])]
    #[case(b"\x1b[31m", vec![Sgr::Fg(Color::Indexed(1))])]
    #[case(b"\x1b[91m", vec![Sgr::Fg(Color::Indexed(9))])]
    #[case(b"\x1b[42m", vec![Sgr::Bg(Color::Indexed(2))])]
    #[case(b"\x1b[39m", vec![Sgr::DefaultFg])]
    #[case(b"\x1b[38;5;196m", vec![Sgr::Fg(Color::Indexed(196))])]
    #[case(b"\x1b[38;2;255;0;0m", vec![Sgr::Fg(Color::Rgb(255, 0, 0))])]
    #[case(b"\x1b[48;5;21m", vec![Sgr::Bg(Color::Indexed(21))])]
    fn sgr(#[case] input: &[u8], #[case] expected: Vec<Sgr>) {
        assert_eq!(feed(input), vec![Command::Sgr(expected)]);
    }

    #[test]
    fn sgr_combines_attribute_and_truecolor() {
        assert_eq!(
            feed(b"\x1b[1;38;2;10;20;30;4m"),
            vec![Command::Sgr(vec![
                Sgr::Bold,
                Sgr::Fg(Color::Rgb(10, 20, 30)),
                Sgr::Underline,
            ])]
        );
    }

    #[test]
    fn a_sequence_split_across_feeds_is_buffered_then_emitted() {
        let mut p = VtParser::new();
        // The CSI is cut mid-sequence: nothing until it completes.
        assert_eq!(p.feed(b"\x1b[31"), vec![]);
        assert_eq!(
            p.feed(b"m"),
            vec![Command::Sgr(vec![Sgr::Fg(Color::Indexed(1))])]
        );
    }

    #[test]
    fn a_lone_esc_waits_for_more() {
        let mut p = VtParser::new();
        assert_eq!(p.feed(b"\x1b"), vec![]);
        assert_eq!(p.feed(b"[A"), vec![Command::CursorUp(1)]);
    }

    #[test]
    fn a_multibyte_char_split_across_feeds_emits_whole() {
        let mut p = VtParser::new();
        // U+4E2D is E4 B8 AD; split after the first byte.
        assert_eq!(p.feed(&[0xE4]), vec![]);
        assert_eq!(
            p.feed(&[0xB8, 0xAD]),
            vec![Command::Print("\u{4E2D}".into())]
        );
    }

    #[test]
    fn an_unhandled_escape_is_consumed_and_dropped() {
        // ESC ( B (designate US ASCII as G0) is recognised-but-unhandled:
        // consumed, no command, including its intermediate byte.
        assert_eq!(feed(b"\x1b(B"), vec![]);
        // And it does not swallow following text.
        assert_eq!(feed(b"\x1b(Bhi"), vec![Command::Print("hi".into())]);
    }

    #[rstest]
    #[case(b"\x1b]0;hello\x07", "hello")]
    #[case(b"\x1b]2;hello\x1b\\", "hello")]
    #[case(b"\x1b]0;\x07", "")]
    fn osc_sets_the_title(#[case] input: &[u8], #[case] expected: &str) {
        assert_eq!(feed(input), vec![Command::SetTitle(expected.to_string())]);
    }

    #[test]
    fn titles_are_stripped_of_control_characters() {
        // A title is untrusted program output; it must not be able to redraw.
        assert_eq!(
            feed(b"\x1b]0;ok\x1b[31mred\x07"),
            vec![Command::SetTitle("ok[31mred".to_string())]
        );
    }

    #[test]
    fn other_osc_commands_are_consumed_without_acting() {
        assert_eq!(feed(b"\x1b]1;icon\x07"), vec![], "icon name alone");
        assert_eq!(feed(b"\x1b]7;file:///tmp\x07"), vec![], "working directory");
        assert_eq!(feed(b"\x1b]10;?\x07"), vec![], "colour query");
    }

    #[test]
    fn an_osc_is_consumed_up_to_its_terminator() {
        // The sequence ends at the BEL: the text after it is ordinary output,
        // not part of the title.
        assert_eq!(
            feed(b"\x1b]0;title\x07after"),
            vec![
                Command::SetTitle("title".to_string()),
                Command::Print("after".into()),
            ]
        );
    }

    #[test]
    fn prelude_path_resolves() {
        // The parser is reachable via prelude::parsers::vt (facade ADR).
        use crate::prelude::parsers::vt::VtParser as P;
        assert!(P::new().feed(b"").is_empty());
    }
}
