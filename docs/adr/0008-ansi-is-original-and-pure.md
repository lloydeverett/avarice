---
status: superseded by [ADR 0011](0011-print-highlights-and-ansi-is-a-core-module.md)
---

# `ansi` is an original module in the Astra-derived crate, and is pure

`ansi` is a **stdlib module**: a table of ANSI escape codes for formatting text on a terminal, and
a few functions for the colours a table cannot hold. It is written entirely in Lua, in
`crates/avarice-rt-stdlib/lua/ansi.lua`, and it is the first stdlib module that is not Astra's.
That makes it nine modules, one Cargo feature (`stdlib-ansi`) and one flag (`StdModules::ANSI`,
`1 << 8`), which is why `StdModules` widens from `u8` to `u16`. The existing bits do not move.

A reader would otherwise wonder why the one file in an Apache-2.0 crate that is not derived from
anything sits beside Astra's, why a module that prints escape codes is registered in a sandbox
when `crypto` is not, and why the `bits()` of a flags type changed width.

## The decision

- **It lives in the stdlib crate.** [ADR 0006](0006-stdlib-derived-from-astra.md) put the stdlib
  in its own crate so that derived code and its licence obligations sit in one unit. `ansi` owes
  Astra nothing, but it is a stdlib module in every other respect (a feature, a flag, a `require`
  name, a profile decides its registration), and a second crate for one file would make "the
  stdlib" mean two places. The crate's boundary is Apache-2.0, and `ansi.lua` says in its header
  that it is original to avarice-rt and is licensed like the crate. It has no `Changes from the
  original:` list, because there is no original. The verbatim rule of ADR 0006 governs Astra's
  files and does not apply to it.
- **It is pure**, and says so where it is defined and documented. This is the first module for
  which it matters: see [ADR 0007](0007-stdlib-modules-are-compile-time-optional.md), by which
  sandbox mode registers the pure modules. `ansi` reaches nothing outside the Lua state, so a
  sandboxed program has it, and every limit the runtime puts on Lua applies to it. Giving it a
  Rust half would move it out of the sandbox, so that is a decision to make on purpose.
- **The codes are constants, and it never looks at the terminal.** Every value is a complete escape
  string (`"\27[1m"`) meant to be concatenated: `ansi.bold .. ansi.fg.red .. "error" ..
  ansi.reset`. The module does not check for a TTY or for `NO_COLOR`. It could not stay pure if it
  did, since that needs the environment, and it would answer differently in one runtime than in
  another for a table that is meant to be data. Whether to emit colour is the program's decision
  or the host's.
- **The surface is small and stays plain.** Styles are flat (`bold`, `dim`, `italic`, `underline`,
  `blink`, `reverse`, `hidden`, `strikethrough`, `reset`), each with a `no_` off code except
  `reset`. Colours are nested under `ansi.fg` and `ansi.bg`: the eight named colours, their
  `bright_` variants and `default`. Those two tables also hold the functions `rgb(r, g, b)`,
  `color256(n)` and `hex(s)`, which raise on an argument that is not an integer in range. Cursor
  movement, erasing, screen control, hyperlinks and a `paint(text, ...)` wrapper are out of scope.
  The tables are ordinary tables, as every other stdlib module's is: an unknown key is `nil`, and
  a program that overwrites one has only itself to blame.

## Considered options

**A separate crate for original modules** was rejected. It would give `avarice-rt` a second path
for stdlib modules to arrive by, a second feature-forwarding scheme, and a second place to look
for what the stdlib contains, all for a file that has no licence obligation to isolate. The cost
of the choice made is one README sentence, and a header that says the file is not Astra's.

**Putting `ansi` in the core**, as `print` is, was rejected. `print` is in the core because every
runtime needs it, sandbox included, and Lua's `print` is what a program expects to find. `ansi` is
something a program asks for by name, and a build that has no use for it should be able to leave
it out, which a feature on a stdlib module does and a piece of the core cannot.

**Detecting the terminal**, and returning empty strings when the output is not a TTY or
`NO_COLOR` is set, was rejected as above: it needs Rust, so the module would no longer be pure,
and it puts a decision in the module that belongs to the program.

**Sandbox registering no module**, which would have left `ansi` out of a sandbox, is the rule
ADR 0007 changes. Withholding `ansi` from a sandboxed program protects nothing and costs it the
escape codes it could have typed out itself.

## Consequences

**`stores` is in the sandbox too.** It was already Lua all the way down, and the rule that lets
`ansi` in lets it in. That is a change to what sandbox mode registers, and is recorded in ADR 0007.

**The count is nine.** ADR 0007 speaks of eight modules, and rightly, for the decision it
records. `stdlib` now turns on nine features, the flags type is `u16`, and `scripts/check-features.sh`
builds eleven combinations: no modules, all of them, and each of the nine alone. Code that
persisted `StdModules::bits()` as a `u8` has to widen it, which is the only break, and it is a
small one: every existing bit has the value it had.

**Purity is data, not a comment.** The stdlib crate records, beside a module's name and feature,
whether it is pure, and `StdModules::PURE` is the pure modules that are compiled in. What sandbox
mode registers is derived from that and not from a second list, so a module cannot be made pure by
being put in one. A test holds the pure modules to having no Rust behind them by checking that
`require` builds each with no `astra_internal__*` global present, and the sandbox tests assert that
every module that is not pure is unreachable.
