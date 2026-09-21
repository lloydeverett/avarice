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
available, `package` and `debug` excepted, and every **stdlib module** is
registered.

Trusted does not mean harmless: `os.exit` ends the host process, `io` reaches
whatever the host user can, and the stdlib modules reach the network and the
filesystem.

## Host module

A module the Rust host registers into a runtime, making it reachable by
`require`. Registered eagerly by value, or lazily by a loader function.

Host modules are the only way capability reaches Lua. Which modules a runtime
has is decided in Rust at construction; Lua code never causes one to load.

## Stdlib module

A **host module** that avarice-rt ships, rather than one an embedder wrote.
`http`, `fs`, `crypto`, `serde`, `datetime`, `utils` and `stores`.

Stdlib modules are host modules like any other, and carry no privilege an
embedder's own module lacks. What distinguishes them is only that a **profile**
decides whether they are registered: **trusted mode** registers them all,
**sandbox mode** registers none.

Named for Lua's standard library by analogy, and separate from it: the standard
library is Lua's own, opened by `mlua`, and reachable without `require`.

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

## Task

Lua work running concurrently with the chunk that spawned it, started from Lua
rather than by the host.

A task is a green thread, not an OS thread: every task shares the one thread its
Lua state lives on, so tasks interleave but never run in parallel and never
observe a half-finished mutation by another. A task outlives the chunk that
spawned it, and nothing ends it for the chunk: the embedder decides, by waiting
for the tasks or aborting them. The runtime keeps no list of them, since their
handles live inside Lua, so it counts and aborts them through the executor.

## Executor

The current-thread tokio runtime a `Runtime` owns. It drives every chunk the
runtime runs and every task Lua spawns, and it runs only while an embedder is
driving it with `Runtime::block_on`. The core owns it rather than leaving the
choice to the embedder. Aborting the runtime's tasks replaces it with a fresh one.

## Write sink

Where `print` sends its output. Owned by the runtime, and replaceable by the
embedder — writing to the host's standard output is the default, not the rule.

## Embedder

A Rust program that depends on avarice-rt as a library to run Lua. `avrt` is
one embedder among others, with no privileged access.
