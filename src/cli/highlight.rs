//! Colours a Lua entry at the prompt as it's typed (ADR 0013).

use std::ops::Range;
use std::sync::{Arc, Mutex};

use nu_ansi_term::{Color, Style};
use reedline::{Highlighter, StyledText};

/// Lua's reserved words, `true`, `false` and `nil` apart: those three are [`Kind::Constant`].
const KEYWORDS: &[&str] = &[
    "and", "break", "do", "else", "elseif", "end", "for", "function", "goto", "if", "in", "local",
    "not", "or", "repeat", "return", "then", "until", "while",
];

/// What a span of source is, for colouring. Not a Lua grammar: there is no operator, punctuation
/// or identifier-role category, only what the palette (ADR 0013) gives a colour to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    /// A reserved word other than `true`, `false` and `nil`.
    Keyword,
    /// `true`, `false` or `nil`.
    Constant,
    /// A short string, or a plain (level-0) long string.
    Str,
    /// A line comment, or a plain (level-0) long comment.
    Comment,
    /// Everything else, numbers included: unstyled.
    Plain,
}

impl Kind {
    fn style(self) -> Style {
        match self {
            Kind::Keyword => Style::new().fg(Color::Red),
            Kind::Constant => Style::new().fg(Color::Magenta),
            Kind::Str => Style::new().fg(Color::Green),
            Kind::Comment => Style::new().dimmed(),
            Kind::Plain => Style::new(),
        }
    }
}

/// Splits `src` into contiguous, colour-classified spans covering every byte of it.
///
/// This is a tokenizer for colouring, not for compiling: it never fails. A string or a comment
/// left open at the end of `src` runs to the end of it rather than being rejected, and a byte
/// that starts nothing recognised is [`Kind::Plain`], the same as a number or an identifier that
/// isn't a keyword. Only plain `[[ … ]]` long brackets are recognised (ADR 0013); `[=[ … ]=]` and
/// deeper levels read as punctuation instead.
fn tokenize(src: &str) -> Vec<(Kind, Range<usize>)> {
    let bytes = src.as_bytes();
    let len = bytes.len();
    let mut spans = Vec::new();
    let mut plain_start: Option<usize> = None;
    let mut i = 0;

    while i < len {
        let b = bytes[i];

        if b == b'-' && bytes.get(i + 1) == Some(&b'-') {
            flush_plain(&mut spans, &mut plain_start, i);
            let start = i;
            i += 2;
            if bytes.get(i) == Some(&b'[') && bytes.get(i + 1) == Some(&b'[') {
                i = find_close(bytes, i + 2);
            } else {
                i = find_eol(bytes, i);
            }
            push_span(&mut spans, Kind::Comment, start..i);
            continue;
        }

        if b == b'[' && bytes.get(i + 1) == Some(&b'[') {
            flush_plain(&mut spans, &mut plain_start, i);
            let start = i;
            i = find_close(bytes, i + 2);
            push_span(&mut spans, Kind::Str, start..i);
            continue;
        }

        if b == b'\'' || b == b'"' {
            flush_plain(&mut spans, &mut plain_start, i);
            let start = i;
            i = find_string_end(bytes, i + 1, b);
            push_span(&mut spans, Kind::Str, start..i);
            continue;
        }

        if b.is_ascii_alphabetic() || b == b'_' {
            flush_plain(&mut spans, &mut plain_start, i);
            let start = i;
            i += 1;
            while i < len && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let word = &src[start..i];
            let kind = if matches!(word, "true" | "false" | "nil") {
                Kind::Constant
            } else if KEYWORDS.contains(&word) {
                Kind::Keyword
            } else {
                Kind::Plain
            };
            push_span(&mut spans, kind, start..i);
            continue;
        }

        if plain_start.is_none() {
            plain_start = Some(i);
        }
        i += 1;
    }
    flush_plain(&mut spans, &mut plain_start, len);
    spans
}

fn flush_plain(spans: &mut Vec<(Kind, Range<usize>)>, plain_start: &mut Option<usize>, end: usize) {
    if let Some(start) = plain_start.take()
        && start < end
    {
        push_span(spans, Kind::Plain, start..end);
    }
}

/// Appends a span, merging it into the previous one when the two are the same kind and touch:
/// numbers and unrecognised words are `Kind::Plain` like the punctuation around them, and a
/// reader should see one run of plain text, not several.
fn push_span(spans: &mut Vec<(Kind, Range<usize>)>, kind: Kind, range: Range<usize>) {
    if let Some((last_kind, last_range)) = spans.last_mut()
        && *last_kind == kind
        && last_range.end == range.start
    {
        last_range.end = range.end;
        return;
    }
    spans.push((kind, range));
}

/// The index right after the next `]]`, or the end of `bytes` if there isn't one.
fn find_close(bytes: &[u8], mut i: usize) -> usize {
    let len = bytes.len();
    while i < len {
        if bytes[i] == b']' && bytes.get(i + 1) == Some(&b']') {
            return i + 2;
        }
        i += 1;
    }
    len
}

/// The index right after the next unescaped `quote`, or at the line's end if there isn't one: a
/// short string does not itself span lines.
fn find_string_end(bytes: &[u8], mut i: usize, quote: u8) -> usize {
    let len = bytes.len();
    while i < len {
        match bytes[i] {
            b'\\' if i + 1 < len => i += 2,
            b'\n' => break,
            b if b == quote => return i + 1,
            _ => i += 1,
        }
    }
    i
}

fn find_eol(bytes: &[u8], i: usize) -> usize {
    let len = bytes.len();
    let mut i = i;
    while i < len && bytes[i] != b'\n' {
        i += 1;
    }
    i
}

/// Colours a REPL entry as Lua.
///
/// `repl::run` keeps the lines of a still-unfinished multi-line entry in a buffer of its own; this
/// shares it (`Highlighter` requires `Send`, so a `Mutex` rather than a `RefCell`), so that a
/// physical line typed while a `--[[` or `[[` opened on an earlier line is still one long comment
/// or string is coloured as such.
pub struct LuaHighlighter {
    prior: Arc<Mutex<String>>,
}

impl LuaHighlighter {
    pub fn new(prior: Arc<Mutex<String>>) -> Self {
        LuaHighlighter { prior }
    }
}

impl Highlighter for LuaHighlighter {
    fn highlight(&self, line: &str, _cursor: usize) -> StyledText {
        let prior = self.prior.lock().unwrap();
        let offset = prior.len();
        let mut full = String::with_capacity(offset + line.len());
        full.push_str(&prior);
        full.push_str(line);
        drop(prior);

        let mut styled = StyledText::new();
        for (kind, range) in tokenize(&full) {
            let start = range.start.max(offset);
            let end = range.end.max(offset);
            if start < end {
                styled.push((kind.style(), full[start..end].to_string()));
            }
        }
        styled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<(Kind, &str)> {
        tokenize(src)
            .into_iter()
            .map(|(kind, range)| (kind, &src[range]))
            .collect()
    }

    #[test]
    fn keywords_are_told_from_constants() {
        assert_eq!(
            kinds("if true then"),
            [
                (Kind::Keyword, "if"),
                (Kind::Plain, " "),
                (Kind::Constant, "true"),
                (Kind::Plain, " "),
                (Kind::Keyword, "then"),
            ]
        );
    }

    #[test]
    fn function_is_a_keyword_like_any_other() {
        assert_eq!(kinds("function"), [(Kind::Keyword, "function")]);
    }

    #[test]
    fn a_word_that_merely_contains_a_keyword_is_not_one() {
        assert_eq!(kinds("endfor"), [(Kind::Plain, "endfor")]);
        assert_eq!(kinds("nilable"), [(Kind::Plain, "nilable")]);
    }

    #[test]
    fn numbers_and_punctuation_are_left_plain() {
        assert_eq!(kinds("x = 3.14 + 0x1F"), [(Kind::Plain, "x = 3.14 + 0x1F")]);
    }

    #[test]
    fn short_strings_are_recognised_with_either_quote() {
        assert_eq!(kinds("'abc'"), [(Kind::Str, "'abc'")]);
        assert_eq!(kinds("\"abc\""), [(Kind::Str, "\"abc\"")]);
    }

    #[test]
    fn an_escaped_quote_does_not_end_the_string() {
        assert_eq!(kinds(r#""a\"b""#), [(Kind::Str, r#""a\"b""#)]);
    }

    #[test]
    fn an_unterminated_short_string_runs_to_the_end_of_its_line() {
        assert_eq!(
            kinds("'abc\ndef"),
            [(Kind::Str, "'abc"), (Kind::Plain, "\ndef")]
        );
    }

    #[test]
    fn a_line_comment_runs_to_the_end_of_the_line_and_no_further() {
        assert_eq!(
            kinds("-- hi\nx"),
            [(Kind::Comment, "-- hi"), (Kind::Plain, "\nx")]
        );
    }

    #[test]
    fn a_plain_long_string_can_span_lines() {
        assert_eq!(kinds("[[a\nb]]"), [(Kind::Str, "[[a\nb]]")]);
    }

    #[test]
    fn a_plain_long_comment_can_span_lines() {
        assert_eq!(kinds("--[[a\nb]]"), [(Kind::Comment, "--[[a\nb]]")]);
    }

    #[test]
    fn an_unterminated_long_bracket_runs_to_the_end_of_the_input() {
        assert_eq!(kinds("--[[a\nb"), [(Kind::Comment, "--[[a\nb")]);
        assert_eq!(kinds("[[a\nb"), [(Kind::Str, "[[a\nb")]);
    }

    #[test]
    fn a_higher_long_bracket_level_is_not_recognised() {
        // `[=[ ... ]=]` is out of scope for v1 (ADR 0013): it reads as punctuation.
        assert_eq!(kinds("[=[x]=]"), [(Kind::Plain, "[=[x]=]")]);
    }

    #[test]
    fn a_lone_bracket_is_plain() {
        assert_eq!(kinds("t[1]"), [(Kind::Plain, "t[1]")]);
    }

    #[test]
    fn multi_byte_characters_inside_a_string_do_not_confuse_the_scan() {
        assert_eq!(kinds("'héllo — 你好'"), [(Kind::Str, "'héllo — 你好'")]);
    }

    fn highlighted(highlighter: &LuaHighlighter, line: &str) -> Vec<(Style, String)> {
        highlighter.highlight(line, 0).buffer
    }

    #[test]
    fn a_fresh_entry_is_highlighted_on_its_own() {
        let highlighter = LuaHighlighter::new(Arc::new(Mutex::new(String::new())));
        let styled = highlighted(&highlighter, "local x = true");
        assert_eq!(
            styled,
            vec![
                (Kind::Keyword.style(), "local".to_string()),
                (Kind::Plain.style(), " x = ".to_string()),
                (Kind::Constant.style(), "true".to_string()),
            ]
        );
    }

    #[test]
    fn a_continuation_line_is_read_in_the_context_of_the_buffer_so_far() {
        // `--[[` opened on the first line; the second line is still inside the comment, although
        // on its own `end` would be a keyword.
        let prior = Arc::new(Mutex::new("--[[\n".to_string()));
        let highlighter = LuaHighlighter::new(prior);
        let styled = highlighted(&highlighter, "end]]");
        assert_eq!(styled, vec![(Kind::Comment.style(), "end]]".to_string())]);
    }

    #[test]
    fn a_string_opened_on_a_prior_line_does_not_leak_across_a_short_string_boundary() {
        // Short strings do not span lines, so a prior unterminated `'` does not carry forward.
        let prior = Arc::new(Mutex::new("'abc\n".to_string()));
        let highlighter = LuaHighlighter::new(prior);
        let styled = highlighted(&highlighter, "end");
        assert_eq!(styled, vec![(Kind::Keyword.style(), "end".to_string())]);
    }
}
