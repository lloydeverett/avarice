---
status: accepted
---

# `avarice` highlights Lua at the prompt

`avarice`'s REPL colours what is typed, not only what is printed. It is called **prompt
highlighting**, deliberately not **highlighting**: that word already means the colour `print`
gives a value it shows ([ADR 0011](0011-print-highlights-and-ansi-is-a-core-module.md)), and the
two are unrelated mechanisms that happen to sit next to each other on the same line.

## The decision

- **What is coloured.** Keywords, `true`/`false`/`nil`, strings (short strings and plain
  `[[ … ]]` long strings), and comments (`--` and plain `--[[ … ]]`). Nothing else: operators,
  punctuation, numbers, and identifiers whether local, global or a call, are left in the
  terminal's own colour. `[=[ … ]=]` and deeper long-bracket levels are out of scope for v1 and
  read as punctuation, the same as they would if long brackets did not exist at all.
- **The palette** reuses `print`'s where the two overlap, and invents the rest:

  | Token | Colour |
  | ----- | ------ |
  | a string | green |
  | `true`, `false`, `nil` | magenta |
  | every other keyword, `function` included | red |
  | a comment | dim (SGR 2) |

  Numbers are left plain, which happens to match `print`'s own palette, where they are plain too.
- **A tolerant, hand-rolled lexer**, not a parser and not a third-party crate. It classifies every
  byte of an entry and never fails: an unterminated string or comment runs to the end of the line
  (a short string) or to the end of the input (a long one) rather than being rejected, and a span
  it cannot make sense of is plain, the same as a number. There is no distinct "this looks broken"
  style; best-effort classification is all a token gets.
- **A multi-line entry is read in the context of what came before it.** `repl::run` already keeps
  the lines of an unfinished entry in a buffer, to know when to compile it; the highlighter is
  given that same buffer, so a `--[[` or `[[` opened two lines up still colours the line being
  typed now as inside the comment or string it opened. `reedline::Highlighter` only ever sees the
  one line it is asked to colour, with no memory of a previous call, so this is the only way it
  can know.
- **`nu-ansi-term` becomes a direct dependency**, pinned to the version `reedline` already resolves
  it to transitively. `reedline` re-exports its `Color` but not its `Style`, and the highlighter
  needs to name `Style` to build one.
- **It lives in `avarice`**, in `src/cli`, behind the `cli` feature, and is not part of the
  library's public surface. A prompt is a concern of the interpreter program, not of the runtime.

## Considered options

**A third-party Lua-parsing crate** was rejected. Such a crate is written to parse a whole,
correct program and reject one that is not; a REPL's job at every keystroke is to colour a program
that is usually incomplete, and asking one of those crates to be tolerant of that is working
against what it is for.

**Full grammar coverage** — operators, punctuation, telling a local from a global from a call —
was rejected for v1 as more than a prompt needs. The four categories above are what a reader scans
for; the rest is not worth the lexer's own complexity yet.

**A distinct style for invalid or unterminated input** was rejected. There is nowhere for it in a
REPL: an entry that will not compile is reported once it is submitted, in the usual way, and
colouring it red while it is still being typed would fire on every unfinished string and every
entry that simply is not finished yet.

**"Syntax colouring"**, avoiding any name that could be confused with `print`'s "highlighting",
was the first name proposed. It was not chosen: "prompt highlighting" was preferred instead, and
this ADR's title and `CONTEXT.md`'s glossary follow that choice.

## Consequences

**A new direct dependency, `nu-ansi-term`.** It was already in the dependency tree, transitively,
through `reedline`; this adds nothing new to what gets built, only a name `avarice` can use
directly.

**`repl::run`'s buffer changes shape.** It moves from a plain `String` to an `Arc<Mutex<String>>`
(`Mutex` rather than `RefCell`, since `reedline::Highlighter` requires `Send`), shared with the
highlighter it hands to `Reedline::with_highlighter`.

**`CONTEXT.md` gains a second, related but distinct, glossary entry.** A reader who knows
"highlighting" only as `print`'s needs "prompt highlighting" defined next to it, not folded into
it.

**A long string or comment above level 0 is not specially recognised.** `[=[return 1]=]` at the
REPL colours as four plain runs of punctuation and text, not as one long string; it still compiles
and runs correctly; only its colouring is plainer than it might be.
