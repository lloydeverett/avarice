---
status: ready-for-agent
---

# The stdlib modules

> **Amended 2026-09-20 and 2026-09-21.** The stdlib crate now holds Astra's sources verbatim, changed only by
> removal and, since a later amendment to ADR 0006, by small additions that alter nothing Astra
> does, and mlua's `send` feature is on. See the amendments to
> [ADR 0004](../adr/0004-async-first-on-tokio.md) and
> [ADR 0006](../adr/0006-stdlib-derived-from-astra.md). Where the sections below disagree with
> them — the `avarice_internal__` rename, `datetime` written on `jiff`, JSON as the only serde
> format, `spawn_local`, a `LocalSet` beside the tokio runtime (dropped on 2026-09-21: with `send`
> on nothing needs one, and it would make `Runtime` `!Send`), `Runtime` staying `!Send`, `crypto.hmac` and `crypto.uuid` (Astra has
> neither; its `uuid` is `utils.uuid`), a `regex` in `utils` (Astra exposes it through
> `validation`, which was not taken and now is: a **`validation` module** makes eight, and the
> regex is `require("validation").regex`), and key/value in `stores` (Astra's has observables and pubsub
> only) — the amendments win.

Expose runtime capability to Lua as eight **stdlib modules**, adapted from
[Astra](https://github.com/ArkForgeLabs/Astra) under Apache-2.0, registered in
**trusted mode** by default and subtractable by the **embedder**.

Decided in [ADR 0004](../adr/0004-async-first-on-tokio.md),
[ADR 0005](../adr/0005-print-implemented-in-lua.md) and
[ADR 0006](../adr/0006-stdlib-derived-from-astra.md). Vocabulary is
[CONTEXT.md](../../CONTEXT.md)'s.

## Problem Statement

avarice-rt runs Lua that can compute but cannot *do* anything.

**Trusted mode** opens `io` and `os`, but those are Lua 5.4's own: `io.open`,
`os.time`, `os.date`, `os.getenv`. There is no way for a **program** to make an
HTTP request, hash or encode a value, parse JSON, list a directory, match a
regular expression, or do two things at once. An **embedder** who wants any of
that writes the **host functions** themselves, in Rust, against mlua — which is
most of the work avarice-rt exists to save them. Every embedder writes the same
modules, differently, and each one is a fresh opportunity to hand Lua a
capability they did not mean to.

Two smaller problems travel with it.

`print` renders a table as `table: 0x55f8c4a1b2c0`. At a REPL, where inspecting
a table is most of what you do, that is the single most common thing to want and
the one thing `print` will not tell you.

And `avrt` has a documented gap: Ctrl-C during a running script is the
terminal's default SIGINT, which kills the process rather than returning to the
prompt. The README says this needs a dependency the crate does not have. It is
about to have one.

## Solution

Eight **stdlib modules**, reachable by `require`:

| Module     | What it is for                                              |
| ---------- | ----------------------------------------------------------- |
| `http`     | An HTTP client: requests, responses, headers, bodies         |
| `fs`       | Files and directories: read, write, list, glob, metadata     |
| `crypto`   | Hashing, HMAC, base64, UUIDs                                 |
| `serde`    | JSON encoding and decoding                                   |
| `datetime` | Instants, civil dates and times, zones, spans, formatting     |
| `utils`    | **Tasks**, `uuid`, `env.get`                                 |
| `stores`   | In-memory key/value, pubsub and observable stores            |
| `validation` | Schema validators, and regular expressions                 |

**Trusted mode** registers all eight; **sandbox mode** registers none. An
embedder who wants trusted-minus-`http` says so on the builder, in the same
shape they already use to subtract a standard library:

```rust
let rt = Runtime::builder(Profile::Trusted)
    .without_std_modules(StdModules::HTTP)
    .build()?;
```

A stdlib module carries no privilege an embedder's own **host module** lacks. It
is registered lazily, so a program that never requires `http` never builds it.

Execution becomes async, because three of the eight are only worth having that
way. `Runtime::exec` and `Runtime::eval` return futures; `Runtime::block_on`
drives them for callers who are not already async. The core owns a current-thread
tokio runtime with a `LocalSet` on it, and Lua spawns **tasks** onto it.

`print` is replaced in both profiles with one that renders a table structurally,
written in Lua over a **write sink** the embedder can redirect.

And `avrt` grows a Ctrl-C handler, closing the gap the README admits to.

## User Stories

### Reaching capability from Lua

1. As a script author, I want to `require("http")` and make a GET request, so that I can call a web API without leaving Lua.
2. As a script author, I want to send POST, PUT, PATCH and DELETE requests with a body and headers, so that I can use an API that is more than reads.
3. As a script author, I want a response object carrying status, headers and body, so that I can branch on what the server actually said rather than only on whether the call threw.
4. As a script author, I want to decode a JSON response body in one call, so that the common case of a JSON API is one step rather than two.
5. As a script author, I want to read and write whole files as strings, so that I can do the thing scripts most often do without `io`'s handle dance.
6. As a script author, I want to list a directory and glob for a pattern, so that I can find the files I mean to work on.
7. As a script author, I want to ask whether a path exists and whether it is a file or a directory, so that I can branch before acting.
8. As a script author, I want to create and remove directories and files, so that a script can lay out its own output.
9. As a script author, I want file metadata — size, modified time — so that I can skip work that is already done.
10. As a script author, I want SHA-2 and SHA-3 hashes and HMAC, so that I can sign a request or verify a payload.
11. As a script author, I want base64 encoding and decoding, so that I can carry binary through a text protocol.
12. As a script author, I want to generate a UUID, so that I can label something uniquely without inventing a scheme.
13. As a script author, I want to encode a Lua table as JSON and decode JSON into a Lua table, so that I can exchange structured data.
14. As a script author, I want JSON decoding to fail with an error I can `pcall`, so that a malformed payload does not take my program down.
15. As a script author, I want the current instant, and a civil date and time in a named zone, so that I can timestamp and schedule work.
16. As a script author, I want to parse and format dates and times, so that I can read what another system produced and produce what it expects.
17. As a script author, I want to add and subtract spans of time, so that "thirty days from now" is one expression.
18. As a script author, I want to compare two instants, so that I can tell which happened first.
19. As a script author, I want to match and replace with regular expressions, so that I can work on text beyond Lua patterns.
20. As a script author, I want to read an environment variable, so that a script can be configured from outside.
21. As a script author, I want in-memory key/value, pubsub and observable stores, so that parts of a program can hand work to each other without me building the plumbing.

### Doing more than one thing

22. As a script author, I want to spawn a **task**, so that a slow HTTP call does not stop the rest of my program.
23. As a script author, I want to spawn work on a timeout and on an interval, so that I can schedule without a loop that sleeps.
24. As a script author, I want to sleep without blocking other tasks, so that waiting is cooperative rather than a stall.
25. As a script author, I want tasks to share my program's globals without tearing them, so that I never have to reason about a half-finished mutation by another task.
26. As an `avrt` user, I want my script to finish its outstanding tasks before the process exits, so that work I spawned is not silently discarded.

### Choosing what is exposed

27. As an embedder, I want trusted mode to register every stdlib module by default, so that the profile means what it says without a list of opt-ins.
28. As an embedder, I want sandbox mode to register none of them, so that untrusted code cannot reach the network or the filesystem through a module I forgot to withhold.
29. As an embedder, I want to take trusted mode and subtract a module, so that I can hand out most of the stdlib without handing out `http`.
30. As an embedder, I want to add a stdlib module to a sandbox runtime deliberately, so that a profile stays a set of defaults rather than a constraint.
31. As an embedder, I want the selection API to look like the one for standard libraries, so that I only learn the shape once.
32. As an embedder, I want a stdlib module to shadow a same-named module in my **module store**, predictably and documentedly, so that I am not surprised by which `fs` I got.
33. As an embedder, I want stdlib modules built on first `require`, so that a runtime I build for a program that uses none of them costs nothing.

### Printing

34. As a script author, I want `print` to show a table's contents rather than its address, so that I can inspect a value at a glance.
35. As a script author, I want `print` to behave the same in sandbox and trusted mode, so that I do not debug against different output than I ship against.
36. As a script author, I want a pathological value — deeply nested, cyclic, enormous — to fail as a catchable Lua error rather than take down the host process, so that printing is never the thing that kills me.
37. As an embedder, I want to redirect where `print` goes, so that I can capture it into a log, a buffer or a socket rather than the host's standard output.
38. As a maintainer, I want the write sink to be settable, so that print behaviour is testable in-process rather than only by inspecting a subprocess's stdout.

### Running it

39. As an embedder, I want one way to run a chunk, so that I cannot accidentally call an async host function under a synchronous entry point and have it hang.
40. As an embedder who is not already async, I want a blocking driver, so that `fn main` and `#[test]` stay simple.
41. As an embedder already inside a tokio runtime, I want the documentation to tell me to build the runtime on its own thread, so that I find out from the docs rather than from a panic.
42. As an `avrt` user, I want Ctrl-C during a running script to abort it and return me to the prompt, so that a mistake costs me a keystroke rather than my session.
43. As an `avrt` user, I want Ctrl-C to say how many tasks it aborted, so that I know whether I lost work in flight.
44. As an `avrt` user, I want Ctrl-D at a prompt to exit cleanly, so that the REPL ends the way every other REPL does.

### Provenance

45. As a maintainer, I want every derived file to name the Astra file it came from and list what changed, so that I can audit our divergence without cloning a repository.
46. As a maintainer, I want the derived code and its licence obligations in one crate, so that the Apache-2.0 boundary is a directory rather than a convention.
47. As a maintainer, I want the stdlib crate to depend on mlua and never on avarice-rt, so that the dependency runs one way.

## Implementation Decisions

### Crate layout

The workspace gains `crates/avarice-rt-stdlib`, and the root `Cargo.toml`
becomes a workspace manifest with the existing package in place. The stdlib
crate carries `LICENSE` (Apache-2.0) and a `README.md` explaining the
derivation. (Amended: there is no `NOTICE`, Astra having none; see the last
amendment to ADR 0006.)

`avarice-rt` depends on it unconditionally, with no feature flag, per ADR 0006.

### The seam between the two crates

One narrow surface, so the core never learns what a module contains:

- `StdModule` — an enum, one variant per module, each knowing its own `require` name.
- `StdModules` — a flags type over `StdModule`, with `ALL` and `NONE`, iterable into its set variants.
- `avarice_rt_stdlib::loader(StdModule) -> impl Fn(&Lua) -> mlua::Result<Value> + 'static` — the module's value, built on demand.

`RuntimeBuilder::build` iterates the selected `StdModules` and hands each
loader to the existing `Runtime::register_lazy_module`. No new registration
mechanism: stdlib modules arrive through the path an embedder's own lazy module
already takes, which is what makes "they carry no privilege yours lacks" true
rather than merely claimed.

`StdModules` is re-exported as `avarice_rt::StdModules`, so an embedder imports
it from one place.

### Selecting modules

Mirrors the existing standard-library API exactly:

- `Profile::std_modules() -> StdModules` — `ALL` for trusted, `NONE` for sandbox.
- `RuntimeBuilder::std_modules(StdModules)` — replace the set.
- `RuntimeBuilder::with_std_modules(StdModules)` — add to it.
- `RuntimeBuilder::without_std_modules(StdModules)` — subtract from it.

No module is refused the way `StdLib::PACKAGE` and `StdLib::DEBUG` are: there is
nothing here that breaks an invariant the runtime rests on, only capability the
embedder is entitled to grant.

`Runtime::has_module` reports a stdlib module as present, since it is registered
as a lazy loader.

### Module shape, and how it differs from Astra's

Astra's two-layer pattern is kept: Rust provides the primitives, a Lua file
bundled with `include_str!` wraps them into the module table. The Lua layer is
where ergonomics live — default arguments, method syntax, error shaping — and
keeping it in Lua keeps it readable.

Astra sets `astra_internal__*` globals from Rust and the Lua layer reads them
off `_G`. That shape is kept. The globals are renamed from `astra_internal__*`
to `avarice_internal__*`, since they are ours now and the old prefix would claim
a project name that is not ours — and that rename is the first
`Changes from the original:` line in every derived file that has a Lua layer.

Because modules are registered lazily, a module's globals appear on first
`require` of that module and not before.

Lua sources are embedded with `include_str!`. No build script, no precompilation
step: `cargo build` on a checkout is the whole toolchain.

### Execution becomes async

Per ADR 0004:

- `Runtime::exec` and `Runtime::eval` return futures. Their signatures are otherwise unchanged.
- `Runtime` owns a `tokio::runtime::Runtime` built with `new_current_thread`, and a `LocalSet`.
- `Runtime::block_on<F: Future>(&self, fut: F) -> F::Output` drives a future on the `LocalSet` to completion, including any tasks spawned during it.
- `block_on` inside another tokio runtime panics, which is tokio's own behaviour and is left alone. The documented answer for an embedder already inside tokio is to build the `Runtime` on its own thread.
- Tasks are spawned with `tokio::task::spawn_local`.
- `Runtime::enter` and the `Execution` guard are unchanged; a time limit arms across awaits, which ADR 0004 accepts.
- mlua gains its `async` feature. Its `send` feature is not enabled, and `Runtime` stays `!Send`.

A pending task is owned by the runtime, so dropping a `Runtime` drops its tasks.
Nothing joins them implicitly; `avrt` waits for them explicitly.

### `print` and the write sink

`print` is replaced in both profiles, in Lua, over one Rust host function that
writes bytes to the sink. Per ADR 0005, the Lua implementation is what bounds it:
recursion is Lua recursion, so depth is a catchable Lua error and the memory cap
covers the buffer it builds.

- `RuntimeBuilder::write_sink(impl Write + 'static)` and `Runtime::set_write_sink(..)`.
- Default is the host's standard output.
- Scalars format as Lua's own `tostring`, so `print(1)` and `print("x")` are unchanged.
- A table renders structurally, with cycles marked rather than followed.
- The sink is flushed per `print` call, so output interleaves correctly with anything else writing to the same stream.

`print` is not a stdlib module and does not live in the stdlib crate: it is core,
because sandbox mode has it and sandbox mode has no stdlib modules.

### `avrt`

- `main` builds the runtime and uses `Runtime::block_on`.
- Script mode runs the program, then waits for every outstanding task before exiting.
- The REPL drains tasks to completion between prompts, so `spawn_interval` holds the terminal until Ctrl-C. This is why the blocking `read_line` stays correct: nothing needs to run while input is awaited.
- A Ctrl-C handler is installed over `tokio::signal::ctrl_c`, selected against the running evaluation. It trips the runtime's `CancelHandle`, which aborts the evaluation and every task, and prints `avrt: aborting N running task(s)` to stderr. *(As built, 2026-09-21: not selected against the evaluation but run on a thread of its own, because a chunk that never awaits would never let the executor read the signal. The handle wakes a waiting chunk and stops a running one itself; see the third amendment to ADR 0004. It prints nothing when no task was running, and exits 130.)*
- Since `avrt` now configures a `CancelHandle` unconditionally, the limit hook is always installed. The comment in `cli/mod.rs` explaining why it deliberately did not is removed along with the behaviour.
- `Ctrl-D` at a prompt exits cleanly, unchanged.
- `--sandbox` gets no stdlib modules, which needs no new flag: it follows from the profile.

### Dependencies

Already approved. Added to `avarice-rt-stdlib` unless noted:

`tokio` (core, with `rt`, `time`, `signal`; `signal` and the driver in `avarice-rt`),
`reqwest`, `sha2`, `sha3`, `base64`, `serde`, `serde_json`, `regex`, `uuid`,
`glob`, `jiff` with its timezone database bundled.

Not taken: `chrono`, `time`, `reqwest-websocket`, `futures`, and the seven
non-JSON serde formats Astra offers (yaml, json5, ini, toml, csv, xml) — one of
which, `serde_yaml`, is published as `0.9.34+deprecated`.

`bitflags` for `StdModules`, approved on the premise that mlua already depends on it,
so it adds nothing to the tree. That premise was wrong: mlua does not. It is in the tree
through `tower-http` (reqwest) and `crossterm` (reedline), so no crate is added, but it
was not mlua that put it there.

`glob` is in on the author's call, against the recommendation to drop it, having
been verified as `rust-lang/glob`, MIT OR Apache-2.0, current at 0.3.4.

### `datetime`, the exception

Written from scratch against `jiff`, on jiff's own type model rather than a
translation of Astra's chrono-shaped one. Six distinct Lua userdata types:
`Zoned`, `Timestamp`, `civil::DateTime`, `civil::Date`, `civil::Time`, `Span`
and `TimeZone`. `SignedDuration` is deliberately not among them — it and `Span`
differ in ways that matter to jiff, calendar units against absolute ones, but
would read as a confusing pair of near-identical Lua types. `Span` covers what
scripts need.

- Metamethods throughout: `__tostring`, `__eq`, `__lt`, `__le`, and `__add`/`__sub` taking a `Span`, so `now + span` reads as arithmetic rather than as a method call.
- `datetime.span{ days = 3 }` replaces Astra's sixteen `add_*`/`sub_*` methods.
- Per-type constructors rather than Astra's single `datetime.new(differentiator, ...)`, which switches on the type of its first argument — `datetime.new(2024)` meaning "the year 2024" while `datetime.new("2024")` means "parse this" is a trap not worth copying into a file we are writing from scratch anyway.
- Astra's `to_locale_date_string`/`to_locale_time_string`/`to_locale_datetime_string` are not carried over: jiff has no locale formatting, and chrono only does it behind an unstable feature.

It carries no Astra attribution header. The crate README says why this file
differs from its neighbours.

### Attribution

Every derived file opens with:

```rust
// Derived from Astra <https://github.com/ArkForgeLabs/Astra> (version 0.51.2, commit
// 885586cca0ef065ac80d6a7c702d05e60fbdbb47), src/components/crypto.rs.
// Copyright 2024 ArkForge LLC, licensed under the Apache License, Version 2.0. See LICENSE in this
// crate's root.
//
// Changes from the original:
//   - <one line per change>
```

`Changes from the original:` is mandatory and never empty. A file copied
verbatim says `none` on that line, and opens "Taken from" where a changed one
opens "Derived from".

### Documentation to update alongside

- `README.md`: a stdlib section, the profile table gaining a stdlib row, the `--path` section gaining the note that bare stdlib names shadow the module store, and the removal of the "one gap worth knowing" paragraph about Ctrl-C.
- `src/lib.rs`'s crate docs: the async entry points, and the doctests that currently call `eval` synchronously.
- `Profile::Trusted`'s doc comment: it now registers stdlib modules.

## Testing Decisions

### What a good test looks like here

A test asserts on what Lua sees, or on what `avrt` prints and exits with. It does
not reach for a Rust function that Lua cannot reach. The existing suite is
already written this way — `tests/sandbox.rs` asks Lua whether `io` is nil rather
than inspecting a `StdLib` value — and that is the standard to hold.

Concretely: a stdlib test drives `require` and calls the module. It does not
call `avarice_rt_stdlib::loader` directly, because an embedder cannot, and a
module that works when called directly but is not reachable through `require` is
broken in the only way that matters.

### The seam

**One primary seam, which already exists:** build a `Runtime`, run Lua source,
assert on the result. Every user story above except the four `avrt` ones is
observable there.

Async changes the harness but not the seam: a test becomes
`rt.block_on(rt.eval::<T>(src, "=test"))`. There is no `#[tokio::test]` anywhere,
because the runtime owns its executor — which is itself worth asserting, since
an embedder gets the same deal.

**One secondary seam, which also already exists:** run the `avrt` binary with
`Command` and assert on stdout, stderr and exit code. Used only for the CLI
stories, as `tests/cli.rs` does today.

The **write sink** is what keeps `print` on the primary seam. Set the sink to a
shared buffer, run Lua, read the buffer — no subprocess, no stdout capture. Test
observability is a reason the sink is settable, not a side effect of it.

Two pieces of new test infrastructure, both in `tests/common/`:

- A **loopback HTTP server**: a `TcpListener` on `127.0.0.1:0` in a thread, serving canned responses and recording what it received. Roughly forty lines, no new dependency, no network. This is what lets `http` be tested at the primary seam. The alternative — a mock-server crate — is a dependency, and the standing rule is to ask first.
- `TempDir` already exists and is the prior art for `fs`. It needs no changes.

### What is tested where

**`tests/stdlib.rs`** — the modules, through `require`.

Per module: the operations in the user stories, their error paths as catchable
Lua errors, and round-trips where the module has an inverse (JSON encode/decode,
base64 encode/decode, datetime parse/format). `crypto` hashes are checked against
published vectors, not against our own output. `http` runs against the loopback
server, including a non-2xx status reaching Lua as a response rather than an
error. `fs` runs under `TempDir`.

**`tests/stdlib.rs`** also covers selection, which is behaviour rather than
configuration:

- Trusted mode: all eight `require` successfully.
- Sandbox mode: all eight fail to `require`, with the runtime's "module not found" message.
- `without_std_modules(HTTP)` on trusted: `http` is gone, the other seven remain.
- `with_std_modules(FS)` on sandbox: `fs` is present, the other seven are not.
- A stdlib module is not built until required — asserted by giving the runtime a store containing a module with the same name and observing which one `require` returns, and by `has_module`.
- A stdlib name shadows a store module of that name.

**`tests/tasks.rs`** — task semantics.

Spawning and joining; a task outliving the chunk that spawned it; interleaving
without tearing a shared table; `sleep` not blocking other tasks; `spawn_timeout`
and `spawn_interval` firing; an error inside a task not killing the runtime;
`block_on` returning only once tasks are done.

**`tests/print.rs`** — `print` and the sink.

Scalars formatted as `tostring` does; a table rendered structurally; a cycle
marked rather than followed; the sink redirected and captured; the sink
replaceable after construction; `print` present and identical in both profiles.

And the case ADR 0005 exists for: a 30 000-deep nested table is printed under the
sandbox's default memory cap and produces **a catchable Lua error, with the
process still alive**. That test is the whole reason `print` is written in Lua,
so it is the one that must not be quietly deleted when it gets slow.

**`tests/sandbox.rs`** — extended, not replaced.

The existing assertions stand. Added: no stdlib module is reachable, and neither
is the network or the filesystem through one.

**`tests/limits.rs`** — extended.

A time limit and a cancel both reach a spawned task, not only the chunk that
spawned it.

**`tests/cli.rs`** — extended.

`avrt` waits for outstanding tasks before exiting; `--sandbox` gets no stdlib
modules; a stdlib module works from a script.

**Unit tests** stay where they are, for things with no Lua-visible surface:
`StdModules` set algebra beside its definition, as `Profile::std_libs` is tested
today; the REPL's `compile` tests, unchanged.

### Known gap

*(Narrowed 2026-09-21.)* Ctrl-C during a running script now has automated tests, because the
race that made them unreliable can be removed: the script prints a line once the handler stands,
and the test sends SIGINT only after reading it. What stays unautomated is Ctrl-C in the REPL,
which needs a terminal and is verified by hand.

## Out of Scope

**Astra modules not taken:** the HTTP server, templating, database and websockets.
*(Validation was on this list, with `regex` to be lifted out of it into `utils`. On
2026-09-21 it was taken whole instead; see the second amendment to ADR 0006.)*

**Serde formats other than JSON.** The module is still called `serde`: it names
what it does, not how many formats it does it in.

**Astra behaviour deliberately dropped:** `env.set` (it wraps `std::env::set_var`,
which is unsound in a process with threads), dotenv loading, `clean_require`,
`close_all_databases`, and the graceful-shutdown/SIGTERM machinery. `utils`
exposes `env.get` and nothing else of the environment.

**An async driver for embedders already inside tokio.** `block_on` panics for
them, as tokio's own `block_on` does, and the documented answer is a dedicated
thread. A
`LocalSet`-based async entry point is a reasonable later addition and needs its
own thought about the runtime's lifetime; ADR 0004 already accepts that
async-std and smol embedders are locked out entirely.

**Reporting the mlua depth issue upstream.** Decided against. ADR 0005 does not
depend on the outcome either way.

**Choosing a licence for the repository root.** Only `crates/avarice-rt-stdlib`
is licensed, because only it has an obligation.

**Publishing.** Both crates stay `publish = false`.

## Further Notes

**Astra's `stores` documentation contradicts its code.** `mem_stores.md:172`
documents `stores.pubsub.subscribe("user:update", user1, function(user) ... end)`
— topic, observable, callback — and `unsubscribe` with the same three. But
`stores.lua:54` implements `PubSub.subscribe(topic, callback)`, with no
observable anywhere in the topic table. The documented calls cannot work. We
ship the two-argument form, the one that exists, and document what we ship.

**`http.lua` is 475 lines of mixed client and server.** Roughly two-thirds is
server, which we do not take. This will be the largest
`Changes from the original:` entry in the crate, and the file most worth reading
carefully after it is cut.

**Astra carries `sha2` at both 0.10.9 and 0.11.0** in its lock file. We take one
version.

**The work is broken into tickets** under
[docs/tickets/stdlib-modules](../tickets/stdlib-modules/), numbered in dependency
order. The ordering lives there rather than here, so there is one place to read
it and one place for it to go stale.

Three tickets have no blockers: the async conversion, the workspace split, and
`print`. The async conversion is the riskiest of the set and the only one whose
diff touches code it does not own.
