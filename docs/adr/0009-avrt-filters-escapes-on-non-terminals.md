---
status: accepted
---

# `avrt` strips escape codes from its output when it is not a colour terminal

> **Amended 2026-09-21; the amendment at the end wins.** `anstream` is replaced by a filter of
> `avrt`'s own, which drops every escape sequence other than colour on a colour terminal too.
`avrt` writes what a program prints through [`anstream`](https://docs.rs/anstream), which passes
ANSI escape codes to a colour terminal and removes them everywhere else: a pipe, a file, a terminal
with `TERM=dumb`, or any destination while `NO_COLOR` is set. Apart from the REPL's prompt, `avrt`
adds no colour of its own; it only decides whether the colour a program wrote is delivered.

A reader would otherwise wonder why `avrt script.lua | cat` prints `red` where the script wrote
`ansi.fg.red .. "red"`, why `print("a\0b")` loses its NUL there, and why `io.write` does not.

This is the decision [ADR 0008](0008-ansi-is-original-and-pure.md) left to "the program". The
`ansi` module never looks at the terminal, and `avrt` is a program.

## The decision

- **What is filtered:** `print`, which reaches the terminal through the runtime's write sink, and
  `avrt`'s own messages on standard output and standard error: the error report (which is also how
  an interrupt or a timeout is reported), the notices about Ctrl-C and about tasks being aborted,
  the `--timeout` usage error, `--version` and the REPL banner. clap's own help and usage errors
  are clap's, and it makes the same decision from the same variables. `avrt` installs an
  `anstream` sink with `RuntimeBuilder::write_sink`, and the library is untouched apart from one
  function it now exports (below).
- **What is not:** `io.write`, `io.stdout` and `io.stderr`. They are C stdio and never reach the
  write sink, so nothing sits between them and the file descriptor. That makes them the way to send
  bytes exactly as they are, and to force colour on where the environment would turn it off.
- **The REPL prompt is told the same decision.** Reedline draws the prompt itself, on standard
  error, and paints with its own defaults without reading the environment, so `avrt` asks
  `anstream` about standard error and passes the answer to `Reedline::with_ansi_colors`. That turns
  off the prompt's colours. It also replaces reedline's default highlighter, which paints typed
  text white and a few example words green, with one that leaves it unstyled: that colour was never
  something a Lua REPL asked for, and white text is unreadable on a light terminal. What reedline
  writes to move the cursor is not colour and is not affected.
- **`print` is a text function.** With colour off, `anstream` keeps printable characters, ASCII
  whitespace and well-formed UTF-8, so a control byte such as NUL is dropped and invalid UTF-8 is
  altered. A program that prints binary writes it with `io.write`. With colour on, `print` passes
  every byte through, as it always has.
- **The environment decides.** Detection is `anstream`'s `auto`: the terminal, `NO_COLOR`,
  `CLICOLOR`, `CLICOLOR_FORCE`, `TERM=dumb` and, on a terminal, `CI`. `NO_COLOR` wins over
  `CLICOLOR_FORCE`, and `CLICOLOR_FORCE` over everything else, a dumb terminal included. The
  `print` sink makes the decision once, when `avrt` starts; `avrt`'s own messages make it as they
  are written, which comes to the same answer. There is no `--color` flag.
- **The flush moves to where it can be shared.** `print` and `io.write` only stay in order on a
  pipe because the sink flushes C stdio before it writes ([ADR 0005](0005-print-implemented-in-lua.md)
  is why `print` is a sink at all). That flush is now the public `avarice_rt::flush_c_stdio`, so
  the `avrt` sink calls the same function the library's default sink does, and the `unsafe` block
  that calls `fflush` stays in one place. An embedder who wraps standard output in a sink of their
  own needs it for the same reason.
- **The dependency is `avrt`'s alone.** `anstream` is an optional dependency behind the `cli`
  feature, so an embedder with `default-features = false` does not build it.

## Considered options

**Leaving reedline alone**, on the ground that it manages its own terminal, was the first plan
and was rejected once it was checked: under `NO_COLOR` the prompt was still coloured. It is not
covered by an automated test, because seeing what reedline writes needs a pseudo-terminal and that
is a dev-dependency this did not seem to justify. It was checked by hand on a pty, with and without
`NO_COLOR` and `CLICOLOR=0`.

**Passing everything through**, as before, was rejected. A program that colours its output is
right to do so on a terminal and wrong to do so into a log file, and every script would have to
learn to tell the two apart with `os.getenv` and no way of asking whether stdout is a terminal.

**A `--color=auto|always|never` flag** was rejected. The environment variables already say the
same thing and are honoured by other tools in the same pipeline, so one setting reaches all of
them, and `CLICOLOR_FORCE=1 avrt script.lua | less -R` does what the flag would. A flag can be
added later if that proves too little, and nothing here would have to change.

**Filtering `io.*` as well**, by giving Lua an `io` whose handles go through the same stream, was
rejected. It is a rewrite of a library that is C all the way down and that programs rely on
behaving like stock Lua's, and it would leave no way to write bytes unfiltered. Leaving `io` raw is
what gives the filter an escape hatch.

**Making `print` binary-safe** by filtering only the escape sequences and letting other bytes
through was rejected, because `anstream` does not offer that on a stream and the alternative is to
strip escape codes ourselves. `print` was already a formatter of values into text (ADR 0005), and
a program that wants bytes has `io.write`.

**Filtering in the library**, so every embedder gets it, was rejected. Whether the process's
standard output is a terminal is a fact about the process, and an embedder's write sink may be a
buffer, a socket or a UI widget that can render escape codes, which would then lose them to a
decision made on its behalf. The library exports the flush it needs to make this possible and
decides nothing.

## Consequences

**`avrt script.lua > file` writes plain text, and so does `avrt script.lua | tool`.** That is the
point, and the thing to know when a script's colour "disappears": `CLICOLOR_FORCE=1` brings it
back, or the script can `io.write` its escapes.

**`print` and `io.write` differ.** A script that prints escape codes with `print` and writes the
same codes with `io.write` gets different output when piped. The README says which is which.

**An unterminated escape sequence carries over.** The stream keeps its parser state between
writes, as a terminal would, so a `print` that ends inside an escape sequence swallows the start of
the next. `ansi`'s codes are always complete strings, so a program has to work to see this.

**The stdlib's own diagnostics are not covered.** `avarice-rt-stdlib` has a few `println!` calls
of its own. That is library code, outside `avrt`, and is left alone.

## Amendment, 2026-09-21: `anstream` is replaced, and more is filtered

[ADR 0010](0010-avrt-filters-terminal-escapes-itself.md) replaces `anstream` with a filter built on
`anstyle-parse`, and it is where the mechanism is described. What it changes here:

- **With colour on, `print` no longer passes every byte through.** Only text and colour do. Every
  other escape sequence, and every control character other than ASCII whitespace, is dropped on a
  colour terminal as it is on a pipe. The "`print` is a text function" decision above now holds in
  both modes.
- **The single-character introducers are dropped.** `anstream` kept a UTF-8 encoded C1 control such
  as U+009B with colour off.
- **An unterminated escape sequence no longer carries over.** The filter forgets at each flush,
  which `print` does after every call.
- **The dependency is `anstyle-parse` and `anstyle-query`**, optional behind `cli` on the terms
  above. `anstream` stays in the tree through clap.

What this ADR decides about the environment, the REPL's prompt, `io.write` and the flush stands.
