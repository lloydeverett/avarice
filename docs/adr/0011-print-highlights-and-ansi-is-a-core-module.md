---
status: accepted
---

# `print` highlights what it prints, and `ansi` is a core module

> [ADR 0012](0012-the-stdlib-is-a-directory-not-a-crate.md) makes the stdlib a directory of
> `avarice-rt` and not a crate, and licenses the whole project Apache-2.0; where the text below
> speaks of the stdlib crate, or says the core carries no licence, it wins.

`print` colours what it shows: the strings of a table, and a function's name, parameters and
address. It does so always, whatever the destination, and it does so in Lua, with the codes from
`ansi`. To make that possible `ansi` moves out of the stdlib crate and into the core, where it is
registered in every runtime, and a function's layout changes to `function (a, b) [0x55d0]`.

This supersedes [ADR 0008](0008-ansi-is-original-and-pure.md) in what it decided about *where*
`ansi` lives and how a build selects it. What that ADR decided about the module itself stands: it
is original, it is pure, its values are constants and it never looks at a terminal.

A reader would otherwise wonder why the library emits escape codes that
[ADR 0009](0009-avrt-filters-escapes-on-non-terminals.md) said it would never decide on, why one
module is registered by the core beside a stdlib that is otherwise all optional, why `StdModules`
has a hole in its bits, and why a runtime without `string` cannot be built.

## The decision

- **The colour is always on.** `print` writes escape codes on every call, and nothing in the
  library asks whether the reader can see them. There is no builder option and no stripping in the
  library. `avrt`'s filter ([ADR 0010](0010-avrt-filters-terminal-escapes-itself.md)) removes the
  codes where they cannot be shown, so a script's output is plain in a pipe and coloured on a
  terminal. An embedder's sink, and the library's default one, receive them as written; one that
  wants plain text wraps its sink.
- **The palette** is fixed and not configurable:

  | What | Colour |
  | ---- | ------ |
  | a string inside a table | green |
  | `function` | red |
  | an address, brackets included, and a `<cycle: …>` marker | dim (SGR 2) |
  | `true`, `false` and `nil`, at the top level and in a table | magenta |

  Everything else is plain: numbers, braces, `=`, commas, indentation, keys, parameter names,
  `table: 0x…`, the text of a thread or userdata, whatever a `__tostring` returns, and a top-level
  string, which is a message and not a literal. A key stays plain whether or not it is bracketed,
  matching the identifiers `avrt`'s prompt highlighting ([ADR 0013](0013-avrt-highlights-lua-at-the-prompt.md))
  leaves uncoloured. Each coloured token ends with a full reset, so no token depends on what came
  before it. `nil` can only be seen at the top level, since a table cannot hold it.
- **A function has one layout, coloured or not**, at the top level and in a table:
  `function (a, b) [0x55d0]`. A function not written in Lua, such as `string.format` or a method on
  a Rust userdata, is `function [0x55d0]`, without parentheses, so that it does not claim to take
  nothing. A function whose debug information was stripped is `function (?, ?, ...) [0x55d0]`. A
  function with a string form of its own keeps it. A function used as a key reads as `tostring`
  gives it, plain with the rest of the key. `tostring` is unchanged.
- **`ansi` is a core module.** The core registers it in every runtime, whatever the profile and
  whatever features the build has, by the path any lazy host module takes, so it carries no
  privilege an embedder's own module lacks. Its source is `src/lua/ansi.lua`. It is no longer a stdlib
  module: `StdModule::Ansi`, `StdModules::ANSI` and the `stdlib-ansi` feature are gone, and
  `stdlib()` lists eight names, which does not include it. `StdModules` stays `u16` and bit 8 is
  left unused, so no other module's bit moves.
- **`print` takes its codes from a table of its own.** The core evaluates `ansi.lua` once when the
  runtime is built and hands the table to `print.lua`, which copies the codes it needs into locals.
  `require("ansi")` evaluates the file again, so a script that overwrites `ansi.fg.red` cannot
  change what `print` emits. The tables are plain, not read-only: that is how every module's are.
- **`string` and `table` are required.** `print.lua` used `if not (string and table)` to fall back
  to a `print` that only writes scalars, for a runtime built without them. With highlighting that is a
  second, quite different `print`, which nothing needs, and `ansi.lua` uses `table` too. The
  fallback is gone, and `RuntimeBuilder::build` refuses a runtime whose libraries lack either,
  saying which, as it refuses `package` and `debug` ([ADR 0002](0002-host-registers-modules.md),
  [ADR 0003](0003-traceback-instead-of-debug.md)). Both profiles include both, so only an embedder
  who removed one meets it.

## Considered options

**Highlighting on request**, a builder option or a `print` argument, was the first recommendation:
the library keeps its rule of deciding nothing, and an embedder opts in. It was rejected because
the colour is meant to be what `print` is, in every runtime, with the destination's filter as the
one place that decides whether it is shown, which `avrt` already is.

**Stripping in the library**, by asking whether standard output is a terminal, was rejected for the
reason ADR 0009 gave: that is a fact about a process, and a sink may be a buffer or a widget that
can show colour.

**Constants in `print.lua`**, leaving `ansi` in the stdlib, was rejected. It duplicates the codes,
and they would drift. The other route, `print` calling `require("ansi")`, fails in a runtime that
does not have the module, and a core function must not depend on an optional one.

**Keeping `ansi` optional and having `print` fall back to plain output without it** was rejected
for the reason the fallback above was: two prints.

**A `string`/`table` fallback that highlights** was rejected. A fallback is the second print again,
and it would need the same libraries anyway.

**Read-only `ansi` tables**, so that a script could not alter them, were rejected. `print` has its
own copy, so nothing needs them, and the stdlib's tables are plain by decision.

**A different function layout when colour is off**, so that plain output kept `function: 0x…(a)`,
was rejected. One layout, read the same wherever it is read, is worth more than the compatibility
of a line nobody was told to parse, and the layout with the address last reads better.

## Consequences

**A sink that cannot show colour shows escape codes.** An embedder that writes `print`'s output to a
log, a socket or a widget gets `ESC [ … m` in it and has to remove it. The library's default
standard-output sink does not remove it either, so a program that embeds the library and leaves
that sink alone writes codes into a pipe. `avrt` is the only embedder that filters. The README says
so in its section on `print`.

**The REPL's results are highlighted**, since they are printed through `print`, and get `avrt`'s
filter like the rest.

**Plain output changed too.** `function: 0x55d0(a, b)` is now `function (a, b) [0x55d0]`, in a
pipe as well as on a terminal. Anything that parsed the old line breaks.

**`StdModules` has a hole.** Bit 8, which was `ANSI`, is unused. It is left alone so that a value
persisted by an embedder that had `ansi` set does not mean another module later, and because the
type is `u16` either way.

**`stdlib-ansi` is gone**, so an embedder that named it in `features` fails to resolve. It is the
only break to features. `scripts/check-features.sh` builds ten combinations: no modules, all of them
and each of the eight alone.

**`ansi.lua` is no longer inside the Apache-2.0 crate.** It was licensed like the crate, and its
header said so. The core carries no licence, and nothing in the file is derived from another
project, so the header now says only that it is original.

**`print` costs a little more.** Every token is a concatenation, on top of the string building it
already did, and it is bounded by the same memory cap. The 30 000-deep table case still ends as a
catchable error and not an abort, and its test stays.

**Two evaluations of `ansi.lua` per runtime that requires it**: one at build for `print`, and one on
the first `require`, which is lazy.

## Amendment, 2026-09-22: an `__index` marker joins the palette

`print` shows `<__index: table>` or `<__index: function>`, dim like `<cycle: …>`, as a pseudo-entry
inside a table's own printed form when its metatable has a non-nil `__index` — not what `__index`
holds, only whether it is a table or a function. The palette gains a row: `<__index: …>` is dim,
alongside `<cycle: …>`.

This exists so that a table whose behaviour lives behind a metatable — an OOP-style module such as
`dirs.app`'s `App` — shows there is more to it than its own fields, without listing methods as
though they were data, which would mix behaviour into what `print` otherwise shows as a view of
data.

It is shown, not run: the check reads `getmetatable(value)` and `rawget` on the result, so
`__index` is never invoked, and a protected metatable (`__metatable`) hides this the same way it
already hides everything else about a table's real metatable — nothing is shown for it either. An
empty table whose metatable has `__index` now prints `{ <__index: table> }` rather than `{}`.
