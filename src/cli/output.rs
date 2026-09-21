//! Where `avrt` writes: standard output and standard error, with escape codes stripped whenever
//! the destination is not a colour terminal (ADR 0009).
//!
//! `anstream` makes that call from the terminal, `NO_COLOR`, `CLICOLOR`, `CLICOLOR_FORCE` and
//! `TERM`. The prompt in the REPL is drawn by reedline and does not come through here; the REPL asks
//! `anstream` for the same decision itself (see `repl::run`).

use std::io::{self, Write};

use anstream::AutoStream;

/// The write sink `avrt` installs: `print`'s output, through the colour filter.
///
/// Like the library's own default sink it flushes C stdio first, so `io.write` output, which
/// bypasses the filter and this sink both, still lands ahead of `print`'s.
pub struct Stdout(AutoStream<io::Stdout>);

impl Stdout {
    /// Decides once, now, whether standard output takes colour.
    pub fn auto() -> Self {
        Stdout(AutoStream::auto(io::stdout()))
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
