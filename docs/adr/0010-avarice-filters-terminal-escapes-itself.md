---
status: accepted
---

# `avarice` filters terminal escapes itself, and lets only text and colour through

`avarice` no longer writes through `anstream`. It runs what a program prints, and its own messages,
through a filter of its own, built on [`anstyle-parse`](https://docs.rs/anstyle-parse), that lets
through printable text and ASCII whitespace, and colour when the destination takes it. Every other
escape sequence, every other control character and every C1 control is dropped, whether or not the
destination takes colour.

This amends [ADR 0009](0009-avarice-filters-escapes-on-non-terminals.md), which had `anstream`
decide whether colour was delivered. It stands by that ADR's decision that the environment decides
and that `io.write` is left raw, and replaces the mechanism and widens what is filtered.

A reader would otherwise wonder why `print("\27[2J")` does nothing even on a colour terminal, why
`avarice` parses escape sequences at all when `anstream` was already there, and why `anstream` is
still in `Cargo.lock` if `avarice` no longer uses it.

## Why `anstream` was not enough

`anstream` answers the question "is colour wanted?" and nothing else. Two things follow from that,
both checked against its source and by running it:

- **With colour on it is a total passthrough.** A program that prints `\27[2J` on a colour terminal
  clears the screen, and `\27]0;title\7` retitles the window. `print` is a text function
  (ADR 0009), and text does not address a cursor.
- **With colour off it misses the single-character introducers.** It strips the seven-bit forms and
  keeps a UTF-8 encoded C1 control such as U+009B, which is CSI as one character. Terminals that
  read it that way, in UTF-8 mode, act on `print("\u{9b}31m")` under `NO_COLOR`. That is the
  reverse of what `NO_COLOR` asks for.

Both are gaps in what a filter *keeps*, and `anstream` offers no way to change that.

## The decision

- **What gets through** is: printable characters (well-formed UTF-8, minus DEL and the C1 controls),
  ASCII whitespace, and, when the destination takes colour, Select Graphic Rendition (`ESC [ … m`).
  Everything else is dropped, and this does not depend on the colour decision: a colour terminal
  is sent colour and text, and a plain one is sent text.
- **Colour is rebuilt, not copied.** A sequence is colour only if it parsed as `ESC [`, parameters,
  and a final `m`, with no intermediate byte or private marker and within the parser's parameter
  limit. It is then written out from what was parsed, with `;` between parameters and `:` between
  subparameters, so only what was understood is sent on.
- **Escape sequences of every other kind are dropped whole**: control sequences (cursor movement,
  erasing, mode changes, queries), operating system commands (window titles, hyperlinks), device
  control strings, the other string introducers, and short two-byte sequences such as `ESC c`. A
  string with a payload is dropped up to its terminator, so what it carried is not left on screen.
- **The single-character introducers are dropped as characters.** U+0080 to U+009F are removed
  from the text without being read as the start of a sequence, so what follows one appears as the
  ordinary text it would be to a terminal that did not read it that way. A lone byte in that range
  is not UTF-8, and is dropped with the other invalid bytes.
- **The parser is `anstyle-parse`'s**, which follows the terminal's own state machine, so that
  the filter and the terminal agree on where a sequence ends. `avarice` supplies what to keep, and a
  small guard in front that hands the parser a multi-byte character only once it is whole: left to
  itself the parser consumes the byte that cuts a character short, which loses a newline, or
  turns an escape into visible text.
- **The environment is decided in `anstream`'s order**, so that `avarice`'s output and clap's help,
  which still goes through `anstream`, always agree: `NO_COLOR` gives none; `CLICOLOR_FORCE` gives
  colour anywhere; `CLICOLOR=0` gives none; otherwise a terminal takes colour if `TERM` is set and
  not `dumb`, or `CLICOLOR=1`, or `CI` is set. The variables are read with `anstyle-query`, the
  crate `anstream` reads them with. The decision is one function, also asked for the REPL's prompt.
- **On Windows the terminal is asked to take escape codes.** Colour is on only if that succeeds. A
  console that cannot take them gets plain text; there is no conversion to the console API.
- **The filter is a stream, and forgets at a flush.** A sequence split across two writes is still
  recognised, so a caller may write in pieces. `flush` resets the parser, which `print` does after
  every call, so each `print` stands alone: one that ends in the middle of an escape sequence does
  not swallow the start of the next.
- **The dependencies are `avarice`'s alone.** `anstyle-parse` and `anstyle-query` are optional,
  behind the `cli` feature, and an embedder with `default-features = false` builds neither.
  `anstream` stays in the lockfile, and in the `avarice` binary, through clap.

## Considered options

**Keeping `anstream`** was rejected for the two gaps above. A wrapper in front of it that removed
the sequences it does not is a filter of its own that then hands the rest to a second one.

**Copying a colour sequence's bytes as they arrived**, having checked that it was colour, was
rejected in favour of writing it out again. The result is the same for every sequence a terminal
would accept, and it means that nothing a parser merely skipped over can be sent on.

**Converting colour for legacy Windows consoles**, as `anstream` does through `anstyle-wincon`,
was rejected. Windows 10 and later take escape codes once asked, and a console too old to would
cost a further dependency and a second code path, for a case nobody has asked for.

**Reading the C1 controls as introducers** and dropping the sequences they begin was rejected. A
terminal that honours them in UTF-8 mode does so for the character, which the filter drops.
Parsing them as sequences too would only decide how much of what follows to swallow, and once the
introducer is gone none of it can act as a sequence, so the parsing buys nothing.

**Letting cursor movement and the like through on a terminal** was rejected. `print` is a way to
write text, and a program that wants to draw with the terminal writes its own bytes with `io.write`.
Nothing in `avarice` needs the exception.

**Passing invalid UTF-8 through**, as `io.write` does, was rejected. `print` is text.

## Consequences

**`print` no longer rings the bell or draws.** `print("\7")` is silent and `print("\27[2J")` is
empty, on a terminal as well as in a pipe. BEL, NUL and the rest of C0 go, as does a hyperlink
(`ESC ] 8`). `ansi` offers none of these (ADR 0008), and `io.write` does not go through the filter.

**Legacy Windows consoles get plain text.** That follows from not converting.

**A sequence split across two `print` calls is not recognised as one.** The parser forgets at each
flush, and the pieces are each taken as they stand: the tail is then ordinary text. Nothing that
`ansi` produces is split, so a program has to build one by hand to see this.

**A few colour sequences are written differently.** A bare `ESC [ m` comes out as `ESC [ 0 m`, an
empty parameter or subparameter comes out as `0`, and a number past 65535 comes out as 65535. The
terminal reads each the same as what was sent.

**Invalid UTF-8 is altered.** A byte that cannot begin a character is dropped. A character cut
short is replaced with U+FFFD, and the byte that cut it short is read as it would have been, so
`print(("é"):sub(1, 1))` still ends in its newline. A character of the right length whose value is
not valid, such as an overlong form, is replaced by the parser, which drops the rest of it. The
output is always well-formed UTF-8, and nothing dropped in this way can act as a control.

**The filter is tested as a whole.** Beside its cases, a test feeds every string of up to four bytes
from an alphabet chosen to start, continue, end and break a sequence, and asserts that what comes out
is text and colour, and is the same whether it is written in one piece or a byte at a time. The
prompt drawn by reedline is still not covered by an automated test (ADR 0009).
