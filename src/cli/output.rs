//! Where `avrt` writes: standard output and standard error, through the escape filter (ADR 0010).
//!
//! Text always gets through, and colour when the destination is a colour terminal; see
//! [`escape`](super::escape). The prompt in the REPL is drawn by reedline and does not come through
//! here; the REPL asks the same question of standard error itself (see `repl::run`).

use std::fmt;
use std::io::{self, IsTerminal, Write};

use super::escape::{takes_colour, Filter};

/// The write sink `avrt` installs: `print`'s output, through the filter.
///
/// Like the library's own default sink it flushes C stdio first, so `io.write` output, which
/// bypasses the filter and this sink both, still lands ahead of `print`'s.
pub struct Stdout(Filter<io::Stdout>);

impl Stdout {
    /// Decides once, now, whether standard output takes colour.
    pub fn auto() -> Self {
        let stdout = io::stdout();
        let colour = takes_colour(&stdout);
        Stdout(Filter::new(stdout, colour))
    }
}

impl Write for Stdout {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        avarice_rt::flush_c_stdio();
        self.0.write(bytes)
    }

    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        avarice_rt::flush_c_stdio();
        self.0.write_all(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

/// Writes one line to standard output, through the filter. What [`println!`] calls.
///
/// A write that fails is ignored, as it is for the error report: there is nowhere left to say so.
pub(super) fn stdout_line(message: fmt::Arguments<'_>) {
    let _ = line(io::stdout(), message);
}

/// Writes one line to standard error, through the filter. What [`eprintln!`] calls.
pub(super) fn stderr_line(message: fmt::Arguments<'_>) {
    let _ = line(io::stderr(), message);
}

/// Writes `message` and a newline to `stream`, having asked whether it takes colour.
///
/// The line is formatted first and written in one piece, so that it is filtered as one and so that
/// nothing else's output lands in the middle of it.
fn line(stream: impl Write + IsTerminal, message: fmt::Arguments<'_>) -> io::Result<()> {
    let colour = takes_colour(&stream);
    write_line(Filter::new(stream, colour), message)
}

fn write_line(mut out: impl Write, message: fmt::Arguments<'_>) -> io::Result<()> {
    let mut text = message.to_string();
    text.push('\n');
    out.write_all(text.as_bytes())?;
    out.flush()
}

/// [`std::println!`], through the filter.
macro_rules! println {
    ($($arg:tt)*) => { $crate::cli::output::stdout_line(format_args!($($arg)*)) };
}

/// [`std::eprintln!`], through the filter.
macro_rules! eprintln {
    ($($arg:tt)*) => { $crate::cli::output::stderr_line(format_args!($($arg)*)) };
}

pub(super) use {eprintln, println};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_is_filtered_and_ends_with_a_newline() {
        let mut out = Vec::new();
        write_line(
            Filter::new(&mut out, false),
            format_args!("a \x1b[31m{}\x1b[0m", "b"),
        )
        .unwrap();
        assert_eq!(out, b"a b\n");
    }

    #[test]
    fn a_line_ending_inside_a_sequence_does_not_swallow_the_next() {
        // Each line is its own write, ended by a flush that forgets what was left unfinished.
        let mut out = Vec::new();
        for text in ["one \x1b[", "two"] {
            let mut sink = Filter::new(&mut out, true);
            write_line(&mut sink, format_args!("{text}")).unwrap();
        }
        assert_eq!(out, b"one \ntwo\n".to_vec());
    }
}
