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
- [Native (C) modules and `-rdynamic`](#native-c-modules-and--rdynamic)
- [Building](#building)

## The `avrt` command

```
avrt [options] [script [args...]]

  -e stat        execute a statement; may be repeated, runs before the script
  -i             enter the REPL after running the script and any -e statements
  -v, --version  print version information
  --sandbox      run in the sandbox profile instead of the trusted one
  --timeout SEC  stop any one script, statement or REPL entry after SEC seconds
  --path DIR     resolve `require` against DIR; may be repeated
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
| `package`             | never                    | never                |
| `debug`               | `traceback` only         | `traceback` only     |
| Binary chunks         | refused                  | allowed              |
| Memory                | 128 MiB                  | unlimited            |
| Time limit            | none unless asked for    | none unless asked for |

Two entries deserve a word.

**`package` is never opened**, in either profile. That is the point of the project rather than an
oversight; see [ADR 0002](docs/adr/0002-host-registers-modules.md).

**`debug` is never opened either.** mlua refuses it on a safe Lua state, because parts of it
(`debug.setmetatable`, `debug.setupvalue`, `debug.upvalueid`) can violate the invariants mlua's
own safety rests on, and the alternative — `Lua::unsafe_new` — is not a trade worth making. In
its place both profiles get a `debug` table holding only `traceback`, which is enough for the
`xpcall(f, debug.traceback)` idiom and needs no library open. Code that feature-detects on
`debug.getinfo` will correctly find it missing.

`Profile::Trusted` is trusted, not harmless: `os.exit` ends the host process, and `io` reads and
writes whatever the host user can.

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

## Native (C) modules and `-rdynamic`

Lua's C ecosystem — `luaposix`, `lpeg`, `lua-cjson` — is reachable, but the host reaches it, not
Lua. `package` is closed, so there is no `package.loadlib` and no searcher that opens a `.so`:
the host calls the module's `luaopen_*` entry point and registers the result, for one profile and
not another.

**If you read one thing in this section, read this:** linking a Rust binary against a *vendored*
Lua and then `dlopen`ing a prebuilt Lua C module does not work by default, and the failure is a
runtime `undefined symbol: lua_gettop`, not a build error. The fix is a linker flag on **your
own binary**, which no dependency can set for you. Details below.

### Why it breaks

A distro-built Lua C module does not link against a Lua library. It is compiled with Lua's
symbols left *undefined*, to be bound from the host process at `dlopen` time — which is why
Debian's `lua-cjson` and `lua-lpeg` depend only on `libc6`. Under the stock `lua` binary those
symbols come from `liblua5.4.so`, which is in the process image.

avarice-rt vendors Lua and links it **statically**. A static library's symbols do not land in the
executable's dynamic symbol table unless the linker is told to put them there, so at `dlopen`
time there is nothing to bind `lua_gettop` to. On this repository's own binary:

```console
$ nm -D --defined-only target/debug/avrt | grep -c 'lua_'
0
$ RUSTFLAGS='-C link-arg=-rdynamic' cargo build
$ nm -D --defined-only target/debug/avrt | grep -c 'lua_'
207
```

### The fix, and why it cannot be ours

On Linux, link the **executable** with `-rdynamic` (the compiler-driver spelling of
`-Wl,--export-dynamic`), which puts the executable's global symbols into the dynamic table:

```rust
// build.rs — in the crate that produces the binary, not in a library it depends on
fn main() {
    #[cfg(unix)]
    println!("cargo::rustc-link-arg-bins=-rdynamic");
}
```

This has to live in your own package. Cargo's link-arg directives apply to the crate being
linked: `rustc-link-arg-bins` affects the binaries of the package whose build script emitted it,
and nothing downstream. **A `build.rs` in avarice-rt could not do this for you**, which is why
this crate does not ship one — it would export 207 symbols from `avrt`, help nobody who depends
on the library, and quietly suggest that the problem was handled. It is a property of the final
link, so it is the final binary's business.

The same applies to a `cdylib`: if your Rust code is itself loaded as a shared library, the Lua
symbols need to be visible from whatever ends up as the process image.

On macOS, `clang` accepts `-rdynamic` and maps it to `-Wl,-export_dynamic`; the same build.rs
line applies. This has not been tested here — only the Linux path above has.

On **Windows there is no equivalent**. A Lua C module `.dll` is linked against `lua54.dll` by
name at build time, so it cannot bind to a Lua that is statically inside your `.exe`. Static
linking, below, is the only path.

### The better answer: link the module statically

Compiling the C module's own sources against the vendored Lua headers avoids `-rdynamic`
entirely, works on Windows, and removes any chance of version skew between the Lua you vendored
and the Lua a distro built the module against.

The headers' location comes from `mlua-sys`, which declares `links = "lua"`. Cargo passes that
metadata to **direct dependents only**, so the embedder needs its own `mlua-sys` dependency —
reaching it through avarice-rt is not enough:

```toml
[dependencies]
avarice-rt = { git = "https://github.com/lloydeverett/avarice-rt", default-features = false }
mlua-sys = { version = "0.12", features = ["lua54", "vendored"] }

[build-dependencies]
cc = "1"
```

```rust
// build.rs
fn main() {
    let include = std::env::var("DEP_LUA_INCLUDE").expect("set by mlua-sys");
    cc::Build::new()
        .file("vendor/lua-cjson/lua_cjson.c")
        .include(include)
        .compile("lua_cjson");
}
```

```rust
use avarice_rt::mlua::{lua_State, Value};
use avarice_rt::{Profile, Runtime};
use std::ffi::c_int;

unsafe extern "C-unwind" {
    fn luaopen_cjson(state: *mut lua_State) -> c_int;
}

let rt = Runtime::new(Profile::Trusted)?;
// Safety: luaopen_cjson is a well-behaved Lua C entry point for this Lua version.
let open = unsafe { rt.lua().create_c_function(luaopen_cjson)? };
let cjson: Value = open.call(())?;
rt.register_module("cjson", cjson)?;
# Ok::<_, avarice_rt::Error>(())
```

Note what is *not* needed here: `Lua::unsafe_new`. That exists to let Lua load C modules, and
Lua never does. `Lua::create_c_function` is `unsafe` — you are vouching for the C — but it
carries no safety-mode check and works on the safe state a `Runtime` is built on.

A last word on trust: a native module runs in your process with your privileges, and nothing
about the sandbox profile contains it. Register one into a sandboxed runtime only if you would
be happy calling it directly from Rust.

## Building

Lua is vendored and built from source by `lua-src` (5.4.9 at the time of writing), so a C
compiler is needed but a system Lua is not.

```console
$ cargo build --release      # the avrt binary and the library
$ cargo test                 # unit and integration tests
$ cargo build --no-default-features   # library only, no clap or reedline
```

## Licence

MIT OR Apache-2.0.
