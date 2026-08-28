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
    EraseInDisplay(EraseMode),
    EraseInLine(EraseMode),
    Sgr(Vec<Sgr>),
    /// A recognised-but-unhandled sequence, already consumed. Never surfaced by
    /// [`VtParser::feed`]; kept as a variant so the parser can report progress.
    Ignored,
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
/// `ST` = `ESC \`). Not acted on yet.
fn parse_osc(input: &[u8]) -> IResult<&[u8], Command> {
    let mut i = 0;
    while i < input.len() {
        match input[i] {
            0x07 => return Ok((&input[i + 1..], Command::Ignored)),
            ESC if i + 1 < input.len() && input[i + 1] == b'\\' => {
                return Ok((&input[i + 2..], Command::Ignored));
            }
            ESC if i + 1 >= input.len() => break, // maybe start of ST; need more
            _ => i += 1,
        }
    }
    Err(Err::Incomplete(Needed::new(1)))
}

fn dispatch_csi(params_bytes: &[u8], final_b: u8) -> Command {
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
        b'J' => Command::EraseInDisplay(erase_mode(param(&params, 0, 0))),
        b'K' => Command::EraseInLine(erase_mode(param(&params, 0, 0))),
        b'm' => Command::Sgr(parse_sgr(&params)),
        _ => Command::Ignored,
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
        // ESC c (full reset) is recognised-but-unhandled: consumed, no command.
        assert_eq!(feed(b"\x1bc"), vec![]);
        // And it does not swallow following text.
        assert_eq!(feed(b"\x1bchi"), vec![Command::Print("hi".into())]);
    }

    #[test]
    fn an_osc_is_consumed_up_to_its_terminator() {
        assert_eq!(
            feed(b"\x1b]0;title\x07after"),
            vec![Command::Print("after".into())]
        );
    }

    #[test]
    fn prelude_path_resolves() {
        // The parser is reachable via prelude::parsers::vt (facade ADR).
        use crate::prelude::parsers::vt::VtParser as P;
        assert!(P::new().feed(b"").is_empty());
    }
}
