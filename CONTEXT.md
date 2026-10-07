# Context

Glossary for avarice. Terms only — no implementation details, no decisions.
Architectural decisions live in `docs/adr/`.

## avarice

The project: a Lua interpreter and embeddable Lua runtime, written in Rust.
Distributed as a Rust library plus a command-line program.

Independent of a separate project by the same author that embeds Luau, which
was also once named avarice; this project was named avarice-rt until it took the
name over. There is no dependency in either direction, and the two do not share
a Lua dialect.

## `avarice`

The command-line program built from avarice. Runs a **program**, or starts
a **REPL**. In backticks, `avarice` means the program; in plain text, the
project.

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
`package`, `debug` — caps memory, and refuses binary chunks and **Lua
finalizers**. Registers no **stdlib module** except the **pure** ones, and every
**core module**.

Sandbox mode describes what avarice itself puts in the runtime. It is not a
guarantee about a runtime an embedder has since reconfigured or added **host
modules** to.

## Trusted mode

The profile for Lua code the author vouches for. The full standard library is
available, `package` and `debug` excepted, and every **stdlib module** that is
**compiled in** is registered.

Trusted does not mean harmless: `os.exit` ends the host process, `io` reaches
whatever the host user can, and the stdlib modules reach the network and the
filesystem.

## Lua finalizer

A function a script gives a table to run when the table is collected: the `__gc`
of the metatable it passes to `setmetatable`.

Not a finalizer of userdata made in Rust, such as a stdlib module's handle to a
running program. Those are the host's, and run whatever the profile.

## Host module

A module the Rust host registers into a runtime, making it reachable by
`require`. Registered eagerly by value, or lazily by a loader function.

Host modules are the only way capability reaches Lua. Which modules a runtime
has is decided in Rust at construction; Lua code never causes one to load.

## Stdlib module

A **host module** that avarice ships, rather than one an embedder wrote or a
**contributed module**.
`http`, `fs`, `crypto`, `serde`, `datetime`, `utils`, `stores`, `validation`,
`dirs` and `process`.

Stdlib modules are host modules like any other, and carry no privilege an
embedder's own module lacks. What distinguishes them is only that a **profile**
decides whether they are registered: **trusted mode** registers every one that
is **compiled in**, **sandbox mode** registers only the **pure** ones.

A stdlib module is in one of three states, and "available" names none of them:
**compiled in** (part of the build), **registered** (a runtime has it, so
`require` finds it), and **loaded** (built by the first `require`). Each implies
the one before.

Named for Lua's standard library by analogy, and separate from it: the standard
library is Lua's own, opened by `mlua`, and reachable without `require`.

## Contributed module

A **host module** that a crate other than avarice defines, for an **embedder**
to add to the runtimes it builds. No **profile** registers one, so a runtime
has it only because the embedder asked.

Unlike a **stdlib module** it is not part of avarice, and avarice does not know
what it is: whether it is **pure**, what it reaches. Being a host module, it
carries no privilege an embedder's own module lacks.

A contributed module cannot take a name a runtime already gives another module.

## Core module

A **host module** the core registers in every runtime, whatever the **profile**,
and that no build can leave out. `ansi` is the only one.

Unlike a **stdlib module** it is not chosen by a profile or by a build, so a
runtime always has it. Being a host module, it carries no privilege an
embedder's own module lacks.

## Pure module

A **host module** written entirely in Lua, with no Rust code behind it,
wherever it lives: `stores` and `ansi`. It only computes over the
values it is given, so it reaches nothing outside the Lua state: no network, no
filesystem, no environment.

Because it is only Lua, every limit a runtime puts on Lua applies to it — the
memory cap, the time limit — which cannot be said of a module with Rust behind
it.

Purity is a fact about how a module is written, not about what it is for.

## Compiled in

A **stdlib module** that is part of an avarice build. Chosen by the
**embedder** when it builds, once, for every runtime in that program. A module
that is not compiled in cannot be **registered**, by a **profile** or by the
embedder.

It says what was built, not what a runtime has: that is **registered**.

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
library, not of `avarice`: an embedder can ask for filesystem resolution without
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
spawned it.

## Executor

The current-thread tokio runtime a `Runtime` owns. It drives every chunk the
runtime runs and every task Lua spawns, and it runs only while an embedder is
driving it with `Runtime::block_on`. The core owns it rather than leaving the
choice to the embedder.

## Write sink

Where `print` sends its output. Owned by the runtime, and replaceable by the
embedder — writing to the host's standard output is the default, not the rule.

## Highlighting

The colour `print` gives the parts of a value it shows: the strings, keys and
commas of a table, a function's name and parameters, and so on.

It is always on. Nothing in the runtime asks whether the reader can see colour,
so what reaches the **write sink** carries it. Whether it reaches a reader is the
destination's concern: `avarice` removes it for a destination that cannot take it,
and an **embedder**'s sink receives it as written.

## Prompt highlighting

The colour `avarice`'s REPL gives what is typed, before it is submitted: keywords, strings and
comments. Distinct from **Highlighting**, which is `print`'s, and applies to a value once it is
shown, not to the source a reader is still typing.

## Embedder

A Rust program that depends on avarice as a library to run Lua. `avarice` is
one embedder among others, with no privileged access.

## Command

A description of a program to run: the program, its arguments, and the
circumstances it runs in — working directory, environment, and what becomes of
each of its standard streams. Describing a Command runs nothing, and one
Command can be run any number of times.

A Command names a program and hands it its arguments one by one. No shell reads
it, so nothing in it is split, quoted, expanded or globbed.

_Avoid_: command line, shell command.

## Child

A program running because Lua started it from a **Command**, together with
whatever of its standard streams were handed to Lua.

Not a **task**: a task is Lua work on the runtime's own thread, and a Child is
another program entirely, which runs in parallel with every task. Like a task,
it outlives the chunk that started it, and ends when its runtime does; it also
outlives Lua's hold on it, so losing every reference to a Child does not end it.

Only the program Lua started is the Child. Programs the Child starts in turn are
its own business, and not Children.

_Avoid_: subprocess, process (for the running thing; `process` names the module).

## Output

What a **Child** leaves behind once it has ended: how it exited, and what it
wrote to its standard output and standard error.

A Child that exits unsuccessfully still has an Output, and so does one killed
for running too long, holding what it wrote before it was killed. A **Command**
that never started has none.

_Avoid_: result.

## Buffer

A sequence of bytes the host holds on Lua's behalf, outside the Lua state, until
Lua asks for them. What an HTTP response's body and a **Child**'s captured
output are.

Bytes, not text: nothing about a Buffer says its contents are UTF-8, or
characters of any encoding. Asked for, it gives them to Lua as a string holding
exactly those bytes, since a Lua string is a sequence of bytes too.

_Avoid_: body (HTTP's word for what a response carries, not for the holder).
