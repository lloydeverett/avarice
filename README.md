# avarice-rt

A Lua 5.4 interpreter and an embeddable Lua runtime, in Rust, built on
[mlua](https://crates.io/crates/mlua).

Two things it adds to mlua: a sandbox that is actually closed, and a module system the host
controls. Lua's `package` library is never opened — `require` is ours, and resolves only what
Rust has registered plus a **module store** the host supplies.

```console
$ avrt
avrt 0.1.0 — Lua 5.4 (trusted profile). Ctrl-D to exit.
> 6 * 7
42
$ avrt --sandbox script.lua
```

```rust
use avarice_rt::{Profile, Runtime};

let rt = Runtime::new(Profile::Sandbox)?;
let answer: i64 = rt.block_on(rt.eval("return 6 * 7", "=example"))?;
# Ok::<_, avarice_rt::Error>(())
```

The vocabulary used throughout — profile, host module, module store, embedder — is defined in
[CONTEXT.md](CONTEXT.md). Decisions that would otherwise be surprising are recorded in
[docs/adr](docs/adr).

## Contents

- [The `avrt` command](#the-avrt-command)
- [Embedding](#embedding)
- [Profiles](#profiles)
  - [Choosing stdlib modules](#choosing-stdlib-modules)
- [Modules](#modules)
- [`print`](#print)
- [Limits](#limits)
- [Building](#building)

## The `avrt` command

```
avrt [options] [script [args...]]

  -e stat        execute a statement; may be repeated, runs before the script
  -i             enter the REPL after running the script and any -e statements
  -v, --version  print version information
  --sandbox      run in the sandbox profile instead of the trusted one
  --timeout SEC  stop any one script, statement or REPL entry, and the wait for its
                 tasks, after SEC seconds
  --path DIR     resolve `require` against DIR instead of the script's own
                 directory; may be repeated, and tried in the order given
  --             end of options
  -              read the script from standard input
```

With no script and no `-e`, `avrt` starts a REPL if standard input is a terminal, and otherwise
reads a program from standard input. `arg` is populated the way stock `lua` does it: `arg[0]` is
the script, `arg[1]` onwards its arguments, and the negative indices walk back through the words
before the script to `arg[-n]`, the interpreter itself.

The command exits 0 on success, 1 on a Lua error, 2 on a usage error, and 130 when Ctrl-C stopped it.

Unlike stock `lua`, `avrt` defaults to the **trusted** profile — you asked for an interpreter, so
you get one — and there is no `-l` flag, because there is no `package.path` for it to search.
Use `--path` and `require`.

### Where `require` looks

`avrt` always gives the runtime a module store, so a script can `require` a sibling file without
being told where to look. With no `--path`, that store is rooted at **the script's own
directory** — or at the current directory when the program comes from standard input or from
`-e`. `--path` *replaces* that root rather than adding to it, and several `--path` options are
tried in the order given.

Under `--sandbox` this widens what a script can run, not what it can reach. A module loaded from
the store is Lua source evaluated in the same runtime under the same profile — no `io`, no `os`,
binary chunks still refused, the same memory cap and timeout — so it is sandboxed exactly as the
entry point is. What the default does give a script is the rest of its own directory: any
`<name>.lua` beside it can be required and run, whether or not you meant it to be part of the
program. Pass `--path` when that set should be a directory you chose rather than wherever the
script happens to sit.

A bare stdlib name shadows a store module of that name: with `crypto.lua` in the script's
directory, `require("crypto")` still returns the stdlib's `crypto`. Name your own modules
something else, or leave the stdlib module out of the runtime.

An embedder using the library gets no store at all unless it asks for one with
`RuntimeBuilder::store`: this default belongs to `avrt`, not to the runtime.

### Tasks and Ctrl-C

A program can leave work running: `utils.spawn_task`, `spawn_timeout` and `spawn_interval` each
start a **task** that carries on after the chunk that spawned it has finished. `avrt` runs the
program and then waits for every outstanding task before it exits, so a task spawned on a
script's last line still runs. A task that spawns another is waited for too. An interval never
finishes by itself, so a script that starts one runs until it is interrupted; a task that should
be abandoned can be stopped with its handle's `abort()`.

**Ctrl-C** stops whatever is running, whether that is a script spinning in a loop, a script
waiting on a response, or the wait for tasks. It aborts every outstanding task, says how many on
stderr (`avrt: aborting 2 running tasks`, and nothing at all if there were none), and exits with
status 130, as a shell reports a process that died of SIGINT. A script that ends in an error, or
by `--timeout`, gives up its tasks the same way rather than waiting for them.

Two limits to know. A script stuck in a call that never returns to Lua, such as `io.read` on a
terminal, notices Ctrl-C only when the call returns; Ctrl-\ (SIGQUIT) still ends it outright.
And because Ctrl-C has to be able to stop anything, `avrt` is always cancellable, which installs
the limit hook, and every run pays a little throughput for it.

### The REPL

Entries are compiled as an expression first, so `6 * 7` prints `42` rather than being a syntax
error; an entry Lua reports as incomplete continues on the next line at a `>>` prompt. Ctrl-D
exits, and history is kept in `$XDG_STATE_HOME/avarice-rt/repl-history`.

Ctrl-C at a prompt abandons the entry being typed, including a half-finished multi-line one.
Ctrl-C while an entry is running abandons the entry and its tasks, as above, and returns to the
prompt with the session intact: globals and loaded modules are as they were.

The REPL waits for the tasks an entry leaves behind before it shows the next prompt, so there is
never anything running at a prompt. The consequence is that spawning an interval holds the
terminal until Ctrl-C, which makes an interval, in the REPL, a foreground command.

## Embedding

```toml
[dependencies]
avarice-rt = { git = "https://github.com/lloydeverett/avarice-rt", default-features = false }
```

`default-features = false` drops clap, reedline and the escape-code parser, which only the `avrt`
binary needs, and it drops the stdlib modules too: they are all compiled in by default, and
`default-features = false` leaves you the ones you name. See [Choosing stdlib modules](#choosing-stdlib-modules).

Running a chunk is asynchronous, because a stdlib module may await while Lua waits for it:
`Runtime::exec` and `Runtime::eval` return futures, and `Runtime::block_on` drives one on the
executor the runtime owns. It panics if called from inside another tokio runtime, so an embedder
that is already async builds its `Runtime` on a thread of its own.

Tasks that Lua spawns run only while a call to `block_on` is in progress, and nothing ends them
when the chunk that spawned them does. `Runtime::outstanding_tasks` says how many there are,
`Runtime::wait_for_tasks` drives them until there are none, and `Runtime::abort_tasks` ends them
all and says how many it ended; it must be called once `block_on` has returned. Dropping the
runtime drops them without waiting. This is what `avrt` is built from.

mlua is a **public dependency**, re-exported as `avarice_rt::mlua`: values, tables and functions
crossing the boundary are mlua's, so an embedder needs the same version avarice-rt was built
against.

## Profiles

A profile is a set of defaults, not a constraint. `RuntimeBuilder` can override anything either
one sets, and doing so never affects the profile another runtime is built from.

|                       | `Profile::Sandbox`       | `Profile::Trusted`   |
| --------------------- | ------------------------ | -------------------- |
| `string` `table` `math` `utf8` `coroutine` | yes | yes     |
| `io`, `os`            | no                       | yes                  |
| `dofile`, `loadfile`  | no                       | yes                  |
| Stdlib modules        | the pure ones            | every one compiled in (all eight by default) |
| `ansi`                | yes                      | yes                  |
| `package`             | never                    | never                |
| `debug`               | `traceback` only         | `traceback` only     |
| Binary chunks         | refused                  | allowed              |
| Memory                | 128 MiB                  | unlimited            |
| Time limit            | none unless asked for    | none unless asked for |

Two entries deserve a word.

**`package` is never opened**, in either profile. That is the point of the project rather than an
oversight; see [ADR 0002](docs/adr/0002-host-registers-modules.md). `RuntimeBuilder` refuses
`StdLib::PACKAGE` outright, so it cannot arrive by accident through `StdLib::ALL_SAFE` — which,
for Lua as opposed to Luau, includes it.

**`debug` is never opened either.** mlua refuses it on a safe Lua state, because parts of it
(`debug.setmetatable`, `debug.setupvalue`, `debug.upvalueid`) can violate the invariants mlua's
own safety rests on, and the alternative — `Lua::unsafe_new` — is not a trade worth making. In
its place both profiles get a `debug` table holding only `traceback`, which is enough for the
`xpcall(f, debug.traceback)` idiom and needs no library open. Code that feature-detects on
`debug.getinfo` will correctly find it missing.

**Stdlib modules** are `http`, `fs`, `crypto`, `serde`, `datetime`, `utils`, `stores` and
`validation`. They are derived from [Astra](https://github.com/ArkForgeLabs/Astra), and kept in
their own Apache-2.0 crate, [`crates/avarice-rt-stdlib`](crates/avarice-rt-stdlib/README.md). They
are registered as lazy host modules, so `require("crypto")` builds `crypto` and a program that
never asks for it costs nothing. A program can ask which it has: `stdlib()` returns a list of the
names to pass to `require`, in a fixed order. It says what this runtime registered, so in a sandbox
it lists only the pure modules, and it is short one module when an embedder took one out. It builds
nothing.

```lua
print(#stdlib())        --> 8, in trusted mode
print(stdlib()[1])      --> http
```

One of the modules, `stores`, is **pure**: written entirely in Lua, with no Rust behind it. A pure
module reaches nothing outside the Lua state, so the memory cap and the time limit govern it like
any Lua a program writes, and a sandbox registers it.

**`ansi` is a core module**, not a stdlib module: the core registers it in every runtime, whatever
the profile and whatever features the build has, because `print` highlights with its codes (see
[`print`](#print)). It is pure Lua too. It is a table of ANSI escape codes
(`ansi.bold .. ansi.fg.red .. "error" .. ansi.reset`) and a few colour functions
(`ansi.fg.rgb(255, 128, 0)`, `ansi.bg.hex("#003366")`, `ansi.fg.color256(202)`), and it is not in
`stdlib()`. It does not check whether the output is a terminal or whether `NO_COLOR` is set; that is
for the program to decide. `avrt` decides by letting only text through what `print` writes, and its
own messages: every escape sequence other than colour is dropped, and so is a control character
such as NUL, and colour is dropped too unless the output is a colour terminal (`NO_COLOR`,
`CLICOLOR`, `CLICOLOR_FORCE` and `TERM=dumb` are honoured). The REPL's prompt is coloured under the
same rule. `io.write` is left alone, so it is the way to write bytes exactly as they are
([ADR 0010](docs/adr/0010-avrt-filters-terminal-escapes-itself.md), which amends
[ADR 0009](docs/adr/0009-avrt-filters-escapes-on-non-terminals.md)). An embedder's write sink gets
`print`'s output as it stands, colour included.

Take trusted mode and subtract one with `without_std_modules`, or add one to a sandbox with
`with_std_modules`. This is a choice made for each runtime, at run time; the next section is the
choice made once, for the whole build.

```rust
use avarice_rt::{Profile, Runtime, StdModules};

let rt = Runtime::builder(Profile::Trusted)
    .without_std_modules(StdModules::HTTP)
    .build()?;
```

### Choosing stdlib modules

Each stdlib module is behind a Cargo feature that compiles it in: `stdlib-http`, `stdlib-fs`,
`stdlib-crypto`, `stdlib-serde`, `stdlib-datetime`, `stdlib-utils`, `stdlib-stores` and
`stdlib-validation`. `stdlib` turns on all eight and is a default feature, so an embedder who
changes nothing gets nothing different. `ansi` has no feature: it is in every build. One who wants
a smaller dependency tree and faster builds names the modules instead:

```toml
[dependencies]
avarice-rt = { git = "https://github.com/lloydeverett/avarice-rt", default-features = false, features = ["stdlib-crypto", "stdlib-serde"] }
```

A feature decides what is **compiled in**; a profile still decides what a runtime **registers**.
`Profile::Trusted` registers every module that is compiled in, `Profile::Sandbox` registers only the pure
modules, and a module that is not compiled in cannot be registered by either or by the embedder. The
feature is not a confinement: Cargo features add up across everything in the dependency graph, so
another crate may turn a module on for you. Keep a module out of a sandbox by not registering it.

`StdModule` and `StdModules` have every variant and flag in every build, so code that names
`StdModules::HTTP` compiles either way. `StdModules::ALL` is the set that is compiled in. Asking
`RuntimeBuilder::build` for a module that is not compiled in is an error that names the feature it
wants; taking one away with `without_std_modules` does nothing, and is not an error.

Some modules borrow another's Rust, and take it whole, because Astra's files are kept as Astra
wrote them. `http` compiles Astra's `serde` code (all its formats) and `validation` compiles its
`utils` code (tasks and `uuid`). Neither registers the module it borrows from: `stdlib-http` alone
gives `require("http")` and nothing else. So `http` does not make the dependency tree as small as
`crypto` does. `scripts/check-features.sh` builds and tests the no-module build, the full build
and each module on its own; see [ADR 0007](docs/adr/0007-stdlib-modules-are-compile-time-optional.md).

`Profile::Trusted` is trusted, not harmless: `os.exit` ends the host process, `io` reads and
writes whatever the host user can, and the stdlib modules reach the network and the filesystem.

Adding a stdlib module that is not pure to a sandbox also gives up part of what the sandbox promises. The memory
cap and the time limit govern Lua; the Rust behind a module is outside both. `validation`'s
`regex(...):captures(s)`, for one, builds its whole result in Rust, at about 220 bytes for each
byte of `s`, so a 16 MiB string costs some 3.5 GB in a sandbox that would refuse a 128 MiB Lua
allocation, and no time limit can interrupt it. See the second amendment to
[ADR 0006](docs/adr/0006-stdlib-derived-from-astra.md).

## Modules

`require` resolves two things, in order: modules the host registered from Rust, then the module
store. There is no `package.path` to point elsewhere, no `package.loadlib`, and no searcher list
to append to.

```rust
use avarice_rt::{FsStore, Profile, Runtime};

let rt = Runtime::builder(Profile::Sandbox)
    .store(FsStore::new("/srv/lua"))   // require("app.util") -> /srv/lua/app/util.lua
    .build()?;

let clock = rt.lua().create_table()?;
clock.set("now", rt.lua().create_function(|_, ()| Ok(0))?)?;
rt.register_module("clock", clock)?;   // require("clock")
# Ok::<_, avarice_rt::Error>(())
```

Module names are validated once, centrally: one or more dot-separated segments of ASCII letters,
digits and underscores, not starting with a digit. `../etc/passwd` is rejected before any store
sees it, which is what lets `FsStore` map segments straight onto path components without
defending itself.

A store answers one question — given a name, produce source or nothing — so the filesystem is
one implementation rather than an assumption:

```rust
use avarice_rt::{ModuleName, ModuleSource, ModuleStore, StoreError};

struct SqliteStore { /* ... */ }

impl ModuleStore for SqliteStore {
    fn fetch(&self, name: &ModuleName) -> Result<Option<ModuleSource>, StoreError> {
        // SELECT source FROM modules WHERE name = ?
        # let _ = name;
        Ok(None)
    }
}
```

`register_lazy_module` defers building a module until something requires it.
`Runtime::set_store` replaces the store after construction. Both host modules and store modules
are cached in Lua's own `_LOADED` table, so `require("x") == require("x")`.

## `print`

Both profiles replace `print` with one that shows a table's contents rather than its address.
Scalars format exactly as `tostring` does, so `print(1)` and `print("x")` are unchanged; a table
prints as indented Lua-like text with its keys in a fixed order, a table that contains itself is
marked `<cycle: ...>` rather than followed, and a table with a `__tostring` metamethod prints
through it, as it does under stock `print`.

A function prints its parameters and then its address, at the top level and inside a table:

```lua
print(require("validation").regex)   --> function (expression) [0x5581c0a4e6f0]
print(function(a, b, ...) end)       --> function (a, b, ...) [0x5581c0a4f120]
print(string.format)                 --> function [0x5581c09b2c10]
```

Only names are shown: Lua has no parameter types, and cannot say that one is optional. A function
written in Rust or C, which is Lua's own library functions and every method on a Rust userdata
such as a compiled regex, has no parameter information at all and prints without parentheses,
rather than claim it takes none. A function whose debug information was stripped shows `?` for each
name, `function (?, ?, ...) [0x…]`. A function with a string form of its own keeps it. `tostring`
is unchanged.

### Highlighting

`print` colours what it shows, in a table and, for a few things, at the top level:

| What                                               | Colour  |
| -------------------------------------------------- | ------- |
| a string inside a table                            | green   |
| the comma after each entry, and between parameters | cyan    |
| a key, brackets and quotes included                | yellow  |
| `function`                                         | red     |
| an address, brackets included, and `<cycle: …>`    | dim     |
| `true`, `false` and `nil`, top level or in a table | magenta |

Everything else is plain: numbers, braces, `=`, a function's parameter names, a top-level string
(which is a message, not a literal), and whatever a value's own `__tostring` says. Each coloured
token ends with a full reset.

The colour is always there. Nothing in the runtime asks whether the reader can see it, so what
reaches the write sink carries escape sequences. `avrt` removes them where they cannot be shown
(a pipe, a file, `NO_COLOR`), so a script's output is plain text there and coloured on a terminal.
An embedder's sink, and the library's default one, receive them as written: one that wants plain
text wraps its sink and strips them. The codes are copied out of `ansi` when the runtime is built,
so a script that edits `require("ansi")` cannot change how `print` highlights
([ADR 0011](docs/adr/0011-print-highlights-and-ansi-is-a-core-module.md)).

`print` needs the `string` and `table` libraries, and `RuntimeBuilder::build` refuses a runtime
without them.

It is written in Lua, so its limits are a Lua program's: a table nested too deeply to print is a
catchable error, bounded by the memory cap, and does not abort the process. The reasons are in
[ADR 0005](docs/adr/0005-print-implemented-in-lua.md).

`print` writes to the runtime's **write sink**, which is standard output unless an embedder
redirects it. Each call writes one line and flushes.

```rust
use avarice_rt::{Profile, Runtime};

let rt = Runtime::builder(Profile::Sandbox)
    .write_sink(std::io::stderr())
    .build()?;
// Or, once built:
rt.set_write_sink(std::io::sink());
# Ok::<_, avarice_rt::Error>(())
```

## Limits

Memory is capped at the allocator, so the cap covers everything Lua allocates rather than only
what the GC can see. Time limits and cancellation are enforced from a debug hook:

```rust
use std::time::Duration;
use avarice_rt::{CancelHandle, Profile, Runtime};

let cancel = CancelHandle::new();
let rt = Runtime::builder(Profile::Sandbox)
    .time_limit(Duration::from_secs(5))
    .cancel_handle(cancel.clone())
    .build()?;

std::thread::spawn(move || cancel.cancel());   // CancelHandle is Send
# Ok::<_, avarice_rt::Error>(())
```

Three details make these hold up against a script that is actively trying to shed them.

The hook is installed with `Lua::set_global_hook`, not `Lua::set_hook`: mlua keys a `set_hook`
callback to the thread that set it, so a script could otherwise shed the limit by doing its work
inside a coroutine.

Tripping **latches**. Once a limit fires, every later check fires too, so catching the error and
carrying on gets the script nowhere. The latch clears when the next top-level execution starts —
and, for cancellation, only once the handle itself is reset.

A cancel stops a runtime that is *waiting* as well as one that is running. A chunk awaiting a
response executes no Lua for the hook to interrupt, so `CancelHandle::cancel` also wakes the
future `Runtime::exec`, `eval` and `run` are waiting on, and the chunk is dropped. `Runtime::run` takes
any future that drives Lua, so it is the way to call a chunk with arguments while keeping this.

The hook triggers on **function calls** as well as on an instruction count. Without that,
`while true do pcall(spin) end` runs forever: the instruction hook nearly always lands inside the
protected call, where `pcall` catches it. The call hook fires as `pcall` is entered, before it
has established its protection, so the latched error propagates out of the loop instead.

A time limit is per top-level execution, not per runtime, and is armed when the future
`Runtime::exec`, `eval` or `run` returns is first polled, or by `Runtime::enter`, and keeps running
while the chunk awaits. If you drive Lua through `Runtime::lua` directly, prefer `Runtime::run`;
failing that, hold an `Execution` guard from `Runtime::enter` for the limit to apply.

## Building

Lua is vendored and built from source by `lua-src` (5.4.9 at the time of writing), so a C
compiler is needed but a system Lua is not.

```console
$ cargo build --release      # the avrt binary and the library
$ cargo test                 # unit and integration tests
$ cargo build --no-default-features   # library only, no clap, reedline or escape-code parser, and no stdlib modules
$ scripts/check-features.sh           # tests with no modules, all of them, and each on its own
```

## Licence

Not chosen yet.
