//! The filter between what a program prints and the terminal (ADR 0010).
//!
//! What gets through is text: printable characters, ASCII whitespace, and, when the destination
//! takes colour, colour. Every other escape sequence, every other control character, and the
//! single-character forms of the introducers (the C1 controls) are dropped, so a program cannot
//! address the cursor, retitle the window, or reach a terminal's more exotic features by printing.
//! (Whitespace, a newline or a tab, still moves it as text does.)
//!
//! The parsing is [`anstyle_parse`]'s, a state machine that follows the terminal's own reading of
//! the bytes. What this module adds is the decision of what to let through, and the environment
//! rules for whether colour is one of the things.

use std::io::{self, IsTerminal, Write};

use anstyle_parse::{Params, Parser, Perform};

/// A writer that lets text through, and colour if it was asked to, and nothing else.
///
/// It is a stream: a sequence may arrive split across writes and is still recognised. That holds
/// until [`flush`](Write::flush), which forgets a sequence left unfinished, so that one write's
/// loose end cannot swallow the start of the next.
pub struct Filter<W> {
    inner: W,
    guard: Utf8Guard,
    parser: Parser,
    keep: Keep,
}

impl<W: Write> Filter<W> {
    pub fn new(inner: W, colour: bool) -> Self {
        Filter {
            inner,
            guard: Utf8Guard::default(),
            parser: Parser::default(),
            keep: Keep {
                colour,
                out: Vec::new(),
            },
        }
    }
}

impl<W: Write> Write for Filter<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.write_all(bytes)?;
        Ok(bytes.len())
    }

    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        for &byte in bytes {
            self.guard.advance(&mut self.parser, &mut self.keep, byte);
        }
        let written = self.inner.write_all(&self.keep.out);
        self.keep.out.clear();
        written
    }

    fn flush(&mut self) -> io::Result<()> {
        self.guard = Utf8Guard::default();
        self.parser = Parser::default();
        self.inner.flush()
    }
}

/// Stands in front of the parser and gives it a multi-byte character only once it is whole.
///
/// The parser reads UTF-8 itself, but when a character is cut short it takes the byte that cut it
/// short along with it: `"\xe4\n"` would lose its newline, and `"\xe4\x1b[31m"` its escape, which
/// would then be shown as text. Here that byte is read afresh instead. A byte that cannot begin a
/// character at all is dropped, so the parser is only ever given characters that are the right
/// length; whether one is a valid code point is still its call.
#[derive(Default)]
struct Utf8Guard {
    held: [u8; 4],
    len: usize,
    /// Continuation bytes still to come.
    wanted: usize,
}

impl Utf8Guard {
    fn advance(&mut self, parser: &mut Parser, keep: &mut Keep, byte: u8) {
        if self.wanted > 0 {
            if matches!(byte, 0x80..=0xbf) {
                self.held[self.len] = byte;
                self.len += 1;
                self.wanted -= 1;
                if self.wanted == 0 {
                    for &held in &self.held[..self.len] {
                        parser.advance(keep, held);
                    }
                }
                return;
            }
            // Cut short. It is replaced through the parser, like any character, so that inside a
            // string being dropped it is dropped with the rest.
            self.wanted = 0;
            for &replacement in "\u{fffd}".as_bytes() {
                parser.advance(keep, replacement);
            }
        }
        match byte {
            0x00..=0x7f => parser.advance(keep, byte),
            0xc2..=0xdf => self.begin(byte, 1),
            0xe0..=0xef => self.begin(byte, 2),
            0xf0..=0xf4 => self.begin(byte, 3),
            // A continuation with nothing to continue, or a byte UTF-8 never uses.
            _ => {}
        }
    }

    fn begin(&mut self, lead: u8, wanted: usize) {
        self.held[0] = lead;
        self.len = 1;
        self.wanted = wanted;
    }
}

/// What the parser found, reduced to what is let through.
struct Keep {
    colour: bool,
    out: Vec<u8>,
}

impl Perform for Keep {
    fn print(&mut self, c: char) {
        // A C1 control that arrived as a character (U+009B is CSI, to a terminal that reads it so)
        // is an introducer in disguise, and DEL is a control that draws nothing. The parser hands
        // over no other control as a character, and one that did would not be text either.
        if !c.is_control() {
            let mut buf = [0; 4];
            self.out
                .extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        }
    }

    fn execute(&mut self, byte: u8) {
        if byte.is_ascii_whitespace() {
            self.out.push(byte);
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: u8) {
        // Only Select Graphic Rendition, and only in its plain form: an intermediate or a private
        // marker makes it some other function, and an overflowed parameter list is not one whose
        // meaning is known.
        if !(self.colour && action == b'm' && intermediates.is_empty() && !ignore) {
            return;
        }
        // Written from the parsed parameters rather than copied, so that only what was understood
        // is sent on.
        self.out.extend_from_slice(b"\x1b[");
        for (i, param) in params.iter().enumerate() {
            if i > 0 {
                self.out.push(b';');
            }
            for (j, value) in param.iter().enumerate() {
                if j > 0 {
                    self.out.push(b':');
                }
                let _ = write!(self.out, "{value}");
            }
        }
        self.out.push(b'm');
    }
}

/// Whether `stream` should be given colour, from the environment and whether it is a terminal.
///
/// This is the decision `anstream` and clap make, in the same order, so that `avarice`'s output and
/// clap's own agree.
pub fn takes_colour(stream: &impl IsTerminal) -> bool {
    let terminal = stream.is_terminal();
    let colour = decide(Environment::read(), terminal);
    // Windows takes colour only once it has been asked to. A console that cannot is not sent any:
    // it would print the escape codes as text, and this does not convert them for it.
    colour && !(terminal && anstyle_query::windows::enable_ansi_colors() == Some(false))
}

/// The variables the decision reads, taken out so that it can be tested without the process's own.
#[derive(Clone, Copy, Default)]
struct Environment {
    no_color: bool,
    clicolor_force: bool,
    clicolor: Option<bool>,
    term_supports_color: bool,
    ci: bool,
}

impl Environment {
    fn read() -> Self {
        Environment {
            no_color: anstyle_query::no_color(),
            clicolor_force: anstyle_query::clicolor_force(),
            clicolor: anstyle_query::clicolor(),
            term_supports_color: anstyle_query::term_supports_color(),
            ci: anstyle_query::is_ci(),
        }
    }
}

fn decide(env: Environment, terminal: bool) -> bool {
    if env.no_color {
        false
    } else if env.clicolor_force {
        true
    } else if env.clicolor == Some(false) {
        false
    } else {
        terminal && (env.term_supports_color || env.clicolor == Some(true) || env.ci)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What `input` comes out as, written in one piece.
    fn whole(input: &[u8], colour: bool) -> Vec<u8> {
        let mut out = Vec::new();
        Filter::new(&mut out, colour).write_all(input).unwrap();
        out
    }

    /// What `input` comes out as, written `size` bytes at a time with no flush between.
    fn in_chunks(input: &[u8], colour: bool, size: usize) -> Vec<u8> {
        let mut out = Vec::new();
        let mut filter = Filter::new(&mut out, colour);
        for chunk in input.chunks(size) {
            filter.write_all(chunk).unwrap();
        }
        out
    }

    /// Asserts what `input` comes out as, and that where the writes fall does not change it.
    #[track_caller]
    fn check(input: &[u8], colour: bool, expected: &[u8]) {
        assert_eq!(
            whole(input, colour),
            expected,
            "colour {colour}: {:?}",
            String::from_utf8_lossy(input)
        );
        for size in 1..=3 {
            assert_eq!(
                in_chunks(input, colour, size),
                expected,
                "colour {colour}, {size} bytes at a time: {:?}",
                String::from_utf8_lossy(input)
            );
        }
    }

    /// Asserts that `input` is dropped whole, with or without colour.
    #[track_caller]
    fn dropped(input: &[u8]) {
        check(input, false, b"");
        check(input, true, b"");
    }

    #[test]
    fn text_passes_in_both_modes() {
        for colour in [false, true] {
            check(b"hello, world", colour, b"hello, world");
            check(
                "héllo — 你好 🎉".as_bytes(),
                colour,
                "héllo — 你好 🎉".as_bytes(),
            );
            check(b"a\tb\nc\r\nd", colour, b"a\tb\nc\r\nd");
        }
    }

    #[test]
    fn a_colour_sequence_passes_only_when_colour_is_on() {
        check(b"\x1b[1;31mred\x1b[0m", true, b"\x1b[1;31mred\x1b[0m");
        check(b"\x1b[1;31mred\x1b[0m", false, b"red");
    }

    #[test]
    fn colour_is_rebuilt_from_what_was_parsed() {
        // A bare `ESC [ m` means reset, and is written out as the zero it stands for.
        check(b"\x1b[m", true, b"\x1b[0m");
        // Extended colours, with their parameters as semicolons and as colons.
        check(b"\x1b[38;2;10;20;30m", true, b"\x1b[38;2;10;20;30m");
        check(b"\x1b[38:2:0:10:20:30m", true, b"\x1b[38:2:0:10:20:30m");
        check(b"\x1b[48;5;200m", true, b"\x1b[48;5;200m");
        // An empty parameter or subparameter is the zero it stands for.
        check(b"\x1b[;31m", true, b"\x1b[0;31m");
        check(b"\x1b[38:2::10:20:30m", true, b"\x1b[38:2:0:10:20:30m");
    }

    #[test]
    fn a_control_sequence_that_is_not_colour_is_dropped() {
        for sequence in [
            &b"\x1b[2J"[..], // erase the screen
            b"\x1b[H",       // move the cursor home
            b"\x1b[10;20H",  // move it elsewhere
            b"\x1b[5A",      // up
            b"\x1b[6n",      // ask where the cursor is
            b"\x1b[?25l",    // hide the cursor
            b"\x1b[?1049h",  // switch to the alternate screen
            b"\x1b[>c",      // ask what the terminal is
            b"\x1b[1;31 m",  // an intermediate before the final byte
            b"\x1b[?1m",     // a private marker on what would be an `m`
            b"\x1b[31$m",    // ditto
        ] {
            dropped(sequence);
        }
    }

    #[test]
    fn a_colour_sequence_with_too_many_parameters_is_dropped() {
        let many = format!("\x1b[{}m", vec!["1"; 40].join(";"));
        dropped(many.as_bytes());
    }

    #[test]
    fn text_around_a_dropped_sequence_is_kept() {
        for colour in [false, true] {
            check(b"a\x1b[2Jb", colour, b"ab");
            check(b"a\x1b]0;title\x07b", colour, b"ab");
        }
    }

    #[test]
    fn an_operating_system_command_is_dropped_whole() {
        // A window title, ended by BEL and by ST; and a hyperlink, which carries text after it.
        dropped(b"\x1b]0;title\x07");
        dropped(b"\x1b]0;title\x1b\\");
        check(b"\x1b]8;;http://x\x07link\x1b]8;;\x07", true, b"link");
        check(b"\x1b]8;;http://x\x07link\x1b]8;;\x07", false, b"link");
    }

    #[test]
    fn a_device_control_string_is_dropped_whole() {
        dropped(b"\x1bP1$r0m\x1b\\");
        dropped(b"\x1bPq#0;2;0;0;0~-\x1b\\");
        check(b"a\x1bP1$r0m\x1b\\b", true, b"ab");
    }

    #[test]
    fn a_short_escape_sequence_is_dropped() {
        // Full reset, index, next line, application keypad, and a character-set designation.
        for sequence in [&b"\x1bc"[..], b"\x1bD", b"\x1bE", b"\x1b=", b"\x1b(0"] {
            dropped(sequence);
        }
        check(b"a\x1bcb", true, b"ab");
    }

    #[test]
    fn the_other_string_introducers_are_dropped_whole() {
        // Start of string, privacy message and application program command.
        dropped(b"\x1bXpayload\x1b\\");
        dropped(b"\x1b^payload\x1b\\");
        dropped(b"\x1b_payload\x1b\\");
    }

    #[test]
    fn a_control_character_other_than_whitespace_is_dropped() {
        for byte in 0u8..0x20 {
            if byte == 0x1b || byte.is_ascii_whitespace() {
                continue;
            }
            dropped(&[byte]);
        }
        dropped(b"\x7f");
        check(b"a\0b\x07c\x08d\x7fe", true, b"abcde");
    }

    #[test]
    fn a_single_character_introducer_is_dropped() {
        // U+009B is CSI as one character. Some terminals honour it, so it must not get through,
        // whether it is a colour or anything else, and whatever follows is not a sequence to it.
        check("a\u{9b}31mb".as_bytes(), true, b"a31mb");
        check("a\u{9b}31mb".as_bytes(), false, b"a31mb");
        check("a\u{9b}2Jb".as_bytes(), true, b"a2Jb");
        // And the rest of C1: NEL, string introducers, OSC, ST.
        for c in '\u{80}'..='\u{9f}' {
            check(c.to_string().as_bytes(), true, b"");
            check(c.to_string().as_bytes(), false, b"");
        }
    }

    #[test]
    fn a_single_byte_introducer_is_dropped() {
        // A lone 0x9B is not UTF-8, and the parser reads the stream as UTF-8, so it is not CSI to
        // it either. It is dropped, and what follows is text.
        for colour in [false, true] {
            check(b"a\x9b31mb", colour, b"a31mb");
            check(b"a\x9b2Jb", colour, b"a2Jb");
            check(b"a\x9d0;title\x07b", colour, b"a0;titleb");
        }
    }

    #[test]
    fn a_byte_that_is_not_utf8_is_dropped() {
        for colour in [false, true] {
            check(b"a\xffb", colour, b"ab");
            check(b"a\x80b", colour, b"ab");
            check(b"a\xc0\x80b", colour, b"ab");
        }
    }

    #[test]
    fn a_character_cut_short_is_replaced_and_the_byte_that_cut_it_is_read_afresh() {
        for colour in [false, true] {
            check(b"a\xe4b", colour, "a\u{fffd}b".as_bytes());
            check(b"a\xc3\nb", colour, "a\u{fffd}\nb".as_bytes());
            check(b"a\xe4\xbd(b", colour, "a\u{fffd}(b".as_bytes());
            // The byte that cut it short may start a sequence, which is then dropped as usual.
            check(b"a\xe4\x1b[2Jb", colour, "a\u{fffd}b".as_bytes());
            check(b"a\xc3\x1b]0;t\x07b", colour, "a\u{fffd}b".as_bytes());
        }
        check(b"a\xe4\x1b[31mb", true, "a\u{fffd}\x1b[31mb".as_bytes());
        // Inside a string that is being dropped, the replacement is dropped with it.
        dropped(b"\x1b]0;t\xc3\x07");
    }

    #[test]
    fn a_character_whose_value_is_not_valid_is_replaced_by_the_parser() {
        // The right length, but an overlong form and a surrogate: what follows is unaffected.
        for colour in [false, true] {
            check(b"a\xe0\x80\x80b", colour, "a\u{fffd}b".as_bytes());
            check(b"a\xed\xa0\x80\x1b[2Jb", colour, "a\u{fffd}b".as_bytes());
        }
    }

    /// Whether `out` is only what the filter promises: text, and colour if `colour`.
    fn is_clean(out: &[u8], colour: bool) -> bool {
        let Ok(text) = std::str::from_utf8(out) else {
            return false;
        };
        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\x1b' && colour {
                // Colour is `ESC [`, digits, `;` and `:`, and `m`.
                if chars.next() != Some('[') {
                    return false;
                }
                loop {
                    match chars.next() {
                        Some('0'..='9' | ';' | ':') => {}
                        Some('m') => break,
                        _ => return false,
                    }
                }
            } else if c.is_control() && !c.is_ascii_whitespace() {
                return false;
            }
        }
        true
    }

    #[test]
    fn nothing_but_text_and_colour_comes_out_whatever_goes_in() {
        // Every string of up to four of these bytes, which are the ones that start, continue,
        // end or break a sequence. The output is clean, and does not depend on how the input is
        // cut into writes.
        const ALPHABET: &[u8] = &[
            0x1b, b'[', b']', b'P', b'X', b'^', b'_', b'\\', b'?', b';', b':', b'0', b'1', b'm',
            b'J', 0x07, 0x18, 0x1a, 0x7f, 0x80, 0x9b, 0xa0, 0xc2, 0xe0, 0xe4, 0xed, 0xf0, 0xff,
        ];
        fn walk(input: &mut Vec<u8>, visit: &mut impl FnMut(&[u8])) {
            visit(input);
            if input.len() == 4 {
                return;
            }
            for &byte in ALPHABET {
                input.push(byte);
                walk(input, visit);
                input.pop();
            }
        }
        walk(&mut Vec::new(), &mut |input| {
            for colour in [false, true] {
                let out = whole(input, colour);
                assert!(
                    is_clean(&out, colour),
                    "{input:?}, colour {colour}: {out:?}"
                );
                assert_eq!(
                    out,
                    in_chunks(input, colour, 1),
                    "{input:?}, colour {colour}"
                );
            }
        });
    }

    #[test]
    fn a_sequence_split_over_writes_is_still_recognised() {
        let mut out = Vec::new();
        let mut filter = Filter::new(&mut out, true);
        filter.write_all(b"a\x1b[3").unwrap();
        filter.write_all(b"1mb\x1b[2").unwrap();
        filter.write_all(b"Jc").unwrap();
        assert_eq!(out, b"a\x1b[31mbc");
    }

    #[test]
    fn a_sequence_left_unfinished_is_forgotten_at_a_flush() {
        for colour in [false, true] {
            for unfinished in [&b"\x1b"[..], b"\x1b[", b"\x1b[31", b"\x1b]0;ti", b"\x1bP1$"] {
                let mut out = Vec::new();
                let mut filter = Filter::new(&mut out, colour);
                filter.write_all(b"a").unwrap();
                filter.write_all(unfinished).unwrap();
                filter.flush().unwrap();
                filter.write_all(b"b").unwrap();
                assert_eq!(out, b"ab", "{unfinished:?}, colour {colour}");
            }
        }
    }

    #[test]
    fn a_multi_byte_character_left_unfinished_is_forgotten_at_a_flush() {
        let mut out = Vec::new();
        let mut filter = Filter::new(&mut out, true);
        filter.write_all(b"a\xe4\xbd").unwrap();
        filter.flush().unwrap();
        filter.write_all(b"b").unwrap();
        assert_eq!(out, b"ab");
    }

    #[test]
    fn write_reports_every_byte_taken() {
        let mut out = Vec::new();
        let mut filter = Filter::new(&mut out, false);
        assert_eq!(filter.write(b"\x1b[31mred").unwrap(), 8);
    }

    fn env(f: impl FnOnce(&mut Environment)) -> Environment {
        let mut env = Environment::default();
        f(&mut env);
        env
    }

    #[test]
    fn a_terminal_takes_colour_if_its_term_does() {
        assert!(decide(env(|e| e.term_supports_color = true), true));
        assert!(!decide(env(|_| {}), true), "TERM unset or dumb");
    }

    #[test]
    fn a_file_or_pipe_takes_none_by_default() {
        assert!(!decide(env(|e| e.term_supports_color = true), false));
    }

    #[test]
    fn clicolor_one_and_ci_turn_colour_on_for_a_terminal_only() {
        assert!(decide(env(|e| e.clicolor = Some(true)), true));
        assert!(decide(env(|e| e.ci = true), true));
        assert!(!decide(env(|e| e.clicolor = Some(true)), false));
        assert!(!decide(env(|e| e.ci = true), false));
    }

    #[test]
    fn clicolor_zero_turns_colour_off() {
        assert!(!decide(
            env(|e| {
                e.term_supports_color = true;
                e.clicolor = Some(false);
            }),
            true
        ));
    }

    #[test]
    fn clicolor_force_turns_colour_on_anywhere() {
        assert!(decide(env(|e| e.clicolor_force = true), false));
        // Including where TERM says otherwise, and where CLICOLOR is 0.
        assert!(decide(
            env(|e| {
                e.clicolor_force = true;
                e.clicolor = Some(false);
            }),
            true
        ));
    }

    #[test]
    fn no_color_beats_everything() {
        assert!(!decide(
            env(|e| {
                e.no_color = true;
                e.clicolor_force = true;
                e.term_supports_color = true;
                e.clicolor = Some(true);
                e.ci = true;
            }),
            true
        ));
    }
}
