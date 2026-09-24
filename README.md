# avarice-rt

An embeddable Lua 5.4 runtime for Rust, built on [mlua](https://crates.io/crates/mlua), and a
command-line interpreter, `avrt`, built on the runtime.

- **Stdlib modules.** `http`, `fs`, `crypto`, `serde`, `datetime`, `utils`, `stores`, `validation`
  and `dirs`, each behind its own Cargo feature. Most are taken from
  [Astra](https://github.com/ArkForgeLabs/Astra).
- **Profiles.** A runtime starts from the sandbox profile or the trusted one, and any setting either
  one makes can be overridden. The sandbox has no `io`, no `os` and no binary chunks, and caps
  memory at 128 MiB. The trusted profile adds `io`, `os` and every stdlib module that is compiled in.
- **Async, on Tokio.** Chunks run as futures on a Tokio runtime, so modules can await I/O, and
  scripts can spawn background tasks.
- **Limits.** Time limits and cancellation apply to Lua running in coroutines and to chunks
  waiting on I/O.
- **The `avrt` command.** Runs scripts, or starts a REPL with history and multi-line input.
- **Host-controlled modules.** Lua's `package` library is never opened. `require` resolves only
  modules registered from Rust and modules in a store the host supplies, such as a directory.

Terms used in the code are defined in [CONTEXT.md](CONTEXT.md). Design decisions are recorded in
[docs/adr](docs/adr).

## Installation

Lua is vendored and built from source, so you need a C compiler but not a system Lua.

To install the `avrt` command:

```console
$ cargo install --git https://github.com/lloydeverett/avarice-rt
```

To use the library, turn off default features so you don't pull in the CLI's dependencies, and
list the stdlib modules you want:

```toml
[dependencies]
avarice-rt = { git = "https://github.com/lloydeverett/avarice-rt", default-features = false, features = ["stdlib-serde"] }
```

Each module has a feature named `stdlib-<name>`; `stdlib` turns on all of them.

## Usage

### The REPL

```console
$ avrt
avrt 0.1.0 — Lua 5.4 (trusted profile). Ctrl-D to exit.
> 6 * 7
42
> { x = 1 }
{
  x = 1,
}
```

### Running a script

```console
$ avrt script.lua arg1 arg2
$ avrt -e 'print(_VERSION)'
$ echo 'print(1 + 1)' | avrt
$ avrt --sandbox --timeout 5 untrusted.lua
```

`avrt` uses the trusted profile unless you pass `--sandbox`. Run `avrt --help` for all options.

### Requiring your own modules

`require` looks in the script's directory by default, or in the directories given with `--path`:

```lua
-- main.lua
local util = require("lib.util")   -- loads lib/util.lua
```

```console
$ avrt main.lua
$ avrt --path ./src --path ./vendor main.lua
```

### Stdlib modules

```lua
local json = require("serde").json
print(json.decode('{"name": "avrt", "tags": ["lua"]}'))

local crypto = require("crypto")
print(crypto.hash("sha2_256", "hello"))
print(crypto.base64.encode("hello"))

local response = require("http").request("https://example.com"):execute()
print(response:status_code(), response:body():bytes())

print(require("dirs").app("myapp", "example.com", "Example"):config())

print(stdlib())   -- the stdlib modules this runtime has
```

The sandbox only registers modules written purely in Lua (currently `stores`). The others reach the
network or filesystem and need the trusted profile.

### Background tasks

```lua
local utils = require("utils")

utils.spawn_timeout(function() print("later") end, 500)
local ticker = utils.spawn_interval(function() print("tick") end, 100)
utils.spawn_timeout(function() ticker:abort() end, 1000)
```

`avrt` waits for outstanding tasks before exiting. Ctrl-C aborts them.

### Embedding

```rust
use avarice_rt::{Profile, Runtime};

let rt = Runtime::new(Profile::Sandbox)?;
let answer: i64 = rt.block_on(rt.eval("return 6 * 7", "=example"))?;
```

### Registering modules and a module store

```rust
use avarice_rt::{FsStore, Profile, Runtime};

let rt = Runtime::builder(Profile::Sandbox)
    .store(FsStore::new("/srv/lua"))   // require("app.util") -> /srv/lua/app/util.lua
    .build()?;

let clock = rt.lua().create_table()?;
clock.set("now", rt.lua().create_function(|_, ()| Ok(0))?)?;
rt.register_module("clock", clock)?;   // require("clock")
```

A store doesn't have to be the filesystem. Implement `ModuleStore` to load source from anywhere:

```rust
use avarice_rt::{ModuleName, ModuleSource, ModuleStore, StoreError};

struct SqliteStore { /* ... */ }

impl ModuleStore for SqliteStore {
    fn fetch(&self, name: &ModuleName) -> Result<Option<ModuleSource>, StoreError> {
        // SELECT source FROM modules WHERE name = ?
        Ok(None)
    }
}
```

### Limits and cancellation

```rust
use std::time::Duration;
use avarice_rt::{CancelHandle, Profile, Runtime};

let cancel = CancelHandle::new();
let rt = Runtime::builder(Profile::Sandbox)
    .time_limit(Duration::from_secs(5))
    .cancel_handle(cancel.clone())
    .build()?;

std::thread::spawn(move || cancel.cancel());
```

### Choosing stdlib modules per runtime

```rust
use avarice_rt::{Profile, Runtime, StdModules};

let trusted_without_http = Runtime::builder(Profile::Trusted)
    .without_std_modules(StdModules::HTTP)
    .build()?;

let sandbox_with_crypto = Runtime::builder(Profile::Sandbox)
    .with_std_modules(StdModules::CRYPTO)
    .build()?;
```

### Redirecting `print`

```rust
use avarice_rt::{Profile, Runtime};

let rt = Runtime::builder(Profile::Sandbox)
    .write_sink(std::io::stderr())
    .build()?;
```

## Licence

Apache License 2.0; see [LICENSE](LICENSE). The stdlib files taken from Astra are under the same
licence, and each has a header saying where it came from and what changed.
