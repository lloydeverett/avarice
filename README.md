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
let answer: i64 = rt.eval("return 6 * 7", "=example")?;
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
- [Limits](#limits)
- [Building](#building)

## The `avrt` command

```
avrt [options] [script [args...]]

  -e stat        execute a statement; may be repeated, runs before the script
  -i             enter the REPL after running the script and any -e statements
  -v, --version  print version information
  --sandbox      run in the sandbox profile instead of the trusted one
  --timeout SEC  stop any one script, statement or REPL entry after SEC seconds
  --path DIR     resolve `require` against DIR instead of the script's own
                 directory; may be repeated, and tried in the order given
  --             end of options
  -              read the script from standard input
```

With no script and no `-e`, `avrt` starts a REPL if standard input is a terminal, and otherwise
reads a program from standard input. `arg` is populated the way stock `lua` does it: `arg[0]` is
the script, `arg[1]` onwards its arguments, and the negative indices walk back through the words
before the script to `arg[-n]`, the interpreter itself.

The command exits 0 on success, 1 on a Lua error, and 2 on a usage error.

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

### The REPL

Entries are compiled as an expression first, so `6 * 7` prints `42` rather than being a syntax
error; an entry Lua reports as incomplete continues on the next line at a `>>` prompt. Ctrl-C
abandons the entry being typed, Ctrl-D exits, and history is kept in
`$XDG_STATE_HOME/avarice-rt/repl-history`.

One gap worth knowing: Ctrl-C during a *running* script is the terminal's default SIGINT, which
kills the process. Interrupting a script back to the prompt needs a signal handler, which needs
a dependency this crate does not yet have. Use `--timeout` in the meantime.

## Embedding

```toml
[dependencies]
avarice-rt = { git = "https://github.com/lloydeverett/avarice-rt", default-features = false }
```

`default-features = false` drops clap and reedline, which only the `avrt` binary needs.

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
| Stdlib modules        | none                     | all seven            |
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

**Stdlib modules** are `http`, `fs`, `crypto`, `serde`, `datetime`, `utils` and `stores`, derived
from [Astra](https://github.com/ArkForgeLabs/Astra) and kept in their own Apache-2.0 crate,
[`crates/avarice-rt-stdlib`](crates/avarice-rt-stdlib/README.md). They are registered as lazy host
modules, so `require("crypto")` builds `crypto` and a program that never asks for it costs nothing.
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

The hook triggers on **function calls** as well as on an instruction count. Without that,
`while true do pcall(spin) end` runs forever: the instruction hook nearly always lands inside the
protected call, where `pcall` catches it. The call hook fires as `pcall` is entered, before it
has established its protection, so the latched error propagates out of the loop instead.

A time limit is per top-level execution, not per runtime, and is armed by `Runtime::exec`,
`Runtime::eval` or `Runtime::enter`. If you drive Lua through `Runtime::lua` directly, hold an
`Execution` guard from `Runtime::enter` for the limit to apply.

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
