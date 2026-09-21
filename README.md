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

`default-features = false` drops clap and reedline, which only the `avrt` binary needs.

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
| Stdlib modules        | none                     | all eight            |
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
`validation`, derived from [Astra](https://github.com/ArkForgeLabs/Astra) and kept in their own
Apache-2.0 crate,
[`crates/avarice-rt-stdlib`](crates/avarice-rt-stdlib/README.md). They are registered as lazy host
modules, so `require("crypto")` builds `crypto` and a program that never asks for it costs nothing.
A program can ask which it has: `stdlib()` returns a list of the names to pass to `require`, in a
fixed order. It says what this runtime registered, so it is empty in a sandbox and short one
module when an embedder took one out, and it builds nothing.

```lua
print(#stdlib())        --> 8, in trusted mode
print(stdlib()[1])      --> http
```

Take trusted mode and subtract one with `without_std_modules`, or add one to a sandbox with
`with_std_modules`:

```rust
use avarice_rt::{Profile, Runtime, StdModules};

let rt = Runtime::builder(Profile::Trusted)
    .without_std_modules(StdModules::HTTP)
    .build()?;
```

`Profile::Trusted` is trusted, not harmless: `os.exit` ends the host process, `io` reads and
writes whatever the host user can, and the stdlib modules reach the network and the filesystem.

Adding a stdlib module to a sandbox also gives up part of what the sandbox promises. The memory
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

A function prints its parameters after its address, at the top level and inside a table:

```lua
print(require("validation").regex)   --> function: 0x5581c0a4e6f0(expression)
print(function(a, b, ...) end)       --> function: 0x5581c0a4f120(a, b, ...)
print(string.format)                 --> function: 0x5581c09b2c10
```

Only names are shown: Lua has no parameter types, and cannot say that one is optional. A function
written in Rust or C, which is Lua's own library functions and every method on a Rust userdata
such as a compiled regex, has no parameter information at all and prints as `tostring` does,
without brackets, rather than claim it takes none. A function whose debug information was
stripped shows `?` for each name. `tostring` is unchanged.

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
$ cargo build --no-default-features   # library only, no clap or reedline
```

## Licence

Not chosen yet.
