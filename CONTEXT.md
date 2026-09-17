# Context

Glossary for avarice-rt. Terms only — no implementation details, no decisions.
Architectural decisions live in `docs/adr/`.

## avarice-rt

The project: a Lua interpreter and embeddable Lua runtime, written in Rust.
Distributed as a Rust library plus a command-line program.

Named after, but independent of, **avarice** — a separate project by the same
author that embeds Luau. There is no dependency in either direction, and the
two do not share a Lua dialect.

## avrt

The command-line program built from avarice-rt. Runs a **program**, or starts
a **REPL**.

The binary is named `avrt` rather than `art` because `art` collides with the
Python `art` package and with Debian's `artemis`.

## Lua

Always means **PUC-Rio Lua, version 5.4** — the reference implementation.

Never means Luau. Luau is a different language: no `require`, no `io`, no
`package`, no integers, no bitwise operators, and a C API incompatible with
PUC-Rio's. Where Luau is meant, it is named explicitly.

## Profile

A named set of defaults for constructing a runtime: which standard libraries
are present, which limits apply. **Sandbox** and **trusted** are the two.

A profile is a starting point, not a constraint. An embedder may reconfigure
anything, including reconfiguring the sandbox profile until it no longer
sandboxes much; that is the embedder's business. Reconfiguring one runtime
never affects the profile any other runtime is built from.

## Sandbox mode

The profile for Lua code the author does not vouch for. Withholds the standard
libraries through which Lua can reach outside its own computation — `io`, `os`,
`package`, `debug` — caps memory, and refuses binary chunks.

Sandbox mode describes what avarice-rt itself puts in the runtime. It is not a
guarantee about a runtime an embedder has since reconfigured or added **host
modules** to.

## Trusted mode

The profile for Lua code the author vouches for. The full standard library is
available, `package` and `debug` excepted.

Trusted does not mean harmless: `os.exit` ends the host process, and `io`
reaches whatever the host user can.

## Host module

A module the Rust host registers into a runtime, making it reachable by
`require`. Registered eagerly by value, or lazily by a loader function.

Host modules are the only way capability reaches Lua. Which modules a runtime
has is decided in Rust at construction; Lua code never causes one to load.

## Native module

A **host module** implemented in C against Lua's C API (`luaposix`, `lpeg`),
rather than in Rust or Lua.

Lua code never loads native code itself: `package` is never opened, so there is
no `package.loadlib` and no searcher that reaches a `.so`. The host compiles or
opens the module and registers it, for one profile and not another.

## Program

Lua source with an entry point, possibly spanning several modules that reach
each other by `require`.

## Module store

Where a **program**'s Lua source lives. Answers one question: given a module
name, produce source or nothing.

Names are hierarchical and dot-separated, as Lua's own `require` names are. The
hierarchy is a naming convention, not a filesystem path — a store is free to
map `foo.bar` onto a directory, a table row, or anything else. Resolution
between names is the runtime's job; the store only fetches.

The filesystem is one store, and currently the only one. It is part of the
library, not of `avrt`: an embedder can ask for filesystem resolution without
going through the command-line program.

## Host function

A function callable from Lua but implemented in Rust. The only way for Lua
code to affect the world outside the interpreter.

## Embedder

A Rust program that depends on avarice-rt as a library to run Lua. `avrt` is
one embedder among others, with no privileged access.
