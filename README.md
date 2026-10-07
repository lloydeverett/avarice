# avarice

An embeddable Lua 5.4 runtime for Rust, built on [mlua](https://crates.io/crates/mlua), and a
command-line interpreter, `avarice`, built on the runtime.

- **Stdlib modules.** `http`, `fs`, `crypto`, `serde`, `datetime`, `utils`, `stores`, `validation`,
  `dirs` and `process`, each behind its own Cargo feature. Most are adapted from
  [Astra](https://github.com/ArkForgeLabs/Astra).
- **Profiles.** A runtime starts from the sandbox profile or the trusted one, and any setting either
  one makes can be overridden. The sandbox has no `io`, no `os`, no binary chunks and no `__gc` in
  a metatable, and caps memory at 128 MiB. The trusted profile adds all four, and every stdlib
  module that is compiled in.
- **Async, on Tokio.** Chunks run as futures on a Tokio runtime, so modules can await I/O, and
  scripts can spawn background tasks.
- **Limits.** A time limit stops Lua that is running, in any coroutine. Time a chunk spends waiting,
  on the network or on another program, counts against it, but the wait itself runs on: the limit
  stops the chunk when it next runs Lua. A cancel stops a chunk that is waiting as well.
- **The `avarice` command.** Runs scripts, or starts a REPL with history and multi-line input.
- **Cross-platform.** Aims to behave the same on Linux, macOS and Windows wherever possible.
- **Host-controlled modules.** Lua's `package` library is never opened. `require` resolves only
  modules registered from Rust and modules in a store the host supplies, such as a directory.

Terms used in the code are defined in [CONTEXT.md](CONTEXT.md). Design decisions are recorded in
[docs/adr](docs/adr).

## Installation

Lua is vendored and built from source, so you need a C compiler but not a system Lua.

```console
$ cargo install --git https://github.com/lloydeverett/avarice
```

## Usage

### The REPL

```console
$ avarice
avarice 0.1.0 — Lua 5.4 (trusted profile). Ctrl-D to exit.
> 6 * 7
42
> { x = 1 }
{
  x = 1,
}
```

### Running a script

```console
$ avarice script.lua arg1 arg2
$ avarice -e 'print(_VERSION)'
$ echo 'print(1 + 1)' | avarice
$ avarice --sandbox --timeout 5 untrusted.lua
$ avarice --path ./src --path ./vendor main.lua
```

`avarice` uses the trusted profile unless you pass `--sandbox`. `require` looks in the script's
directory, or in the `--path` directories. Run `avarice --help` for all options.

## Stdlib

```lua
print(stdlib())   -- the stdlib modules this runtime has
```

The sandbox only registers `stores` and the core module `ansi`, which are pure Lua. The other
modules need the trusted profile.

### `http`

```lua
local http = require("http")

local res = http.request("https://httpbin.org/get"):execute()
print(res:status_code(), res:headers(), res:body():json())

local res = http.request({
  url = "https://httpbin.org/post",
  method = "POST",
  headers = { ["x-token"] = "abc" },
  body = { hello = "world" },   -- tables are sent as JSON
}):execute()
print(res:status_code() == http.status_codes.OK)
```

### `fs`

```lua
local fs = require("fs")

fs.create_dir_all("out/logs")
fs.write_file("out/logs/a.txt", "hello")
print(fs.read_file("out/logs/a.txt"), fs.exists("out/logs/a.txt"))

for _, entry in ipairs(fs.read_dir("out/logs")) do
  print(entry:file_name(), entry:type():is_file())
end
print(fs.get_metadata("out/logs/a.txt"):last_modified())
print(fs.parse_glob("out/**/*.txt").entries)

fs.remove_dir_all("out")
```

### `crypto`

```lua
local crypto = require("crypto")

print(crypto.hash("sha2_256", "hello"))   -- also sha2_512, sha3_256, sha3_512
print(crypto.base64.encode("hello"), crypto.base64.decode("aGVsbG8="))
print(crypto.base64.encode_urlsafe("hello?"))
```

### `serde`

```lua
local serde = require("serde")

local cfg = serde.json.decode('{"name": "avarice", "tags": ["lua"]}')
print(serde.json.encode(cfg))
print(serde.yaml.encode(cfg))
print(serde.toml.decode("port = 8080").port)
print(serde.csv.decode("a,b\n1,2\n").body)
-- also json5, ini, xml
```

### `datetime`

```lua
local dt = require("datetime")   -- jiff's types and methods; see https://docs.rs/jiff

local now = dt.Zoned.now()
print(now, now:year(), now:weekday())
print(dt.date(2026, 1, 31) + dt.span { months = 1 })   -- 2026-02-28
print(dt.datetime(2026, 3, 9, 9, 30):in_tz("America/New_York"))
print(dt.Timestamp.parse("2026-01-15T00:00:00Z"):span_until(dt.Timestamp.now()))   -- jiff's `until`
print(dt.date(2026, 9, 24):strftime("%A %-d %B %Y"))   -- Thursday 24 September 2026
```

### `utils`

```lua
local utils = require("utils")

print(utils.uuid(), utils.env.get("HOME"))

local task = utils.spawn_task(function() print("in the background") end)
task:await()

utils.spawn_timeout(function() print("later") end, 500)
local ticker = utils.spawn_interval(function() print("tick") end, 100)
utils.spawn_timeout(function() ticker:abort() end, 1000)
```

`avarice` waits for outstanding tasks before exiting. Ctrl-C aborts them.

### `stores`

```lua
local stores = require("stores")

local count = stores.observable(0)
count:subscribe(function(n) print("count is", n) end)
count:publish(1)

stores.pubsub.subscribe("greet", function(name, topic) print(topic, name) end)
stores.pubsub.publish("greet", "world")
```

### `validation`

```lua
local validation = require("validation")
local T = validation.types

local User = T.build(T.struct({
  name = T.string(),
  age = T.optional(T.integer()),
  role = T.string({ default = "user" }),
}))
print(User({ name = "ada" }).role)   -- user
print(pcall(User, { name = 1 }))     -- false  name: expected string, got number

local re = validation.regex([[(\d+)-(\d+)]])
print(re:is_match("10-20"), re:replace("10-20", "$2-$1"))
```

### `dirs`

```lua
local app = require("dirs").app("myapp", "Example", "com")
print(app:config(), app:data(), app:cache())   -- e.g. ~/.config/myapp on Linux
print(app:state(), app:runtime())              -- nil where the platform has none
```

### `process`

```lua
local process = require("process")

-- No shell: the program and each argument are entries of their own, passed as they are.
local output = process.run({ "git", "log", "--oneline", "-5", cwd = "some/repo" })
print(output.ok, output.code, output.stdout:bytes(), output.stderr:bytes())

-- Raise on failure, or on taking too long; the error carries what was captured.
local ok, err = pcall(process.run, { "make", "test", check = true, timeout = 60000 })
if not ok then print(err, err.kind, err.output and err.output.stderr:bytes()) end

-- Talk to a Child while it runs.
local child = process.spawn({ "sort" })
child.stdin:write("pear\napple\n")
child.stdin:close()
for line in child.stdout:lines() do print(line) end
print(child:wait().code)
```

A running Child counts as a task: `avarice` waits for it before exiting, and Ctrl-C kills it. Only
the Child is killed, not programs it started. `--timeout` does not cut a wait for a Child short; a
Command's own `timeout` does.

### `ansi`

A core module: always present, in every profile and build.

```lua
local ansi = require("ansi")

print(ansi.bold .. ansi.fg.red .. "error" .. ansi.reset)
print(ansi.fg.hex("#ff8800") .. ansi.bg.color256(236) .. "orange" .. ansi.reset)
```

## Embedding

Turn off default features to leave out the CLI's dependencies, then list the stdlib modules you
want. Each module is a feature named `stdlib-<name>`; `stdlib` turns on all of them. A module left
out isn't compiled, and neither are its dependencies.

```toml
[dependencies]
avarice = { git = "https://github.com/lloydeverett/avarice", default-features = false, features = [
  "stdlib-serde",
  "stdlib-crypto",
] }

# features = []            smallest: the core runtime, `print` and `ansi`, no stdlib modules
# features = ["stdlib"]    every module; `stdlib-http` is the heaviest (reqwest, rustls)
```

```rust
use std::time::Duration;

use avarice::mlua::{self, Lua, Value};
use avarice::{
    CancelHandle, FsStore, HostModule, ModuleName, ModuleSource, ModuleStore, Profile, Runtime,
    StdModules, StoreError,
};

fn main() -> avarice::Result<()> {
    let cancel = CancelHandle::new();

    // Start from a profile: `Sandbox` (no io, os, binary chunks or `__gc`; 128 MiB memory cap)
    // or `Trusted` (all of those, and every stdlib module compiled in). Any setting can be
    // overridden.
    let rt = Runtime::builder(Profile::Sandbox)
        // Add or remove stdlib modules, from those the build's features compiled in.
        .with_std_modules(StdModules::SERDE | StdModules::CRYPTO)
        // Add a module another crate contributes: `require("greeting")`.
        .module(Greeting)
        // Stop Lua once 5 seconds have passed, the next time it runs, or when `cancel.cancel()` is
        // called from any thread, even while it waits.
        .time_limit(Duration::from_secs(5))
        .cancel_handle(cancel.clone())
        // `require("app.util")` loads /srv/lua/app/util.lua.
        .store(FsStore::new("/srv/lua"))
        // Send `print` somewhere other than stdout.
        .write_sink(std::io::stderr())
        .build()?;

    // Expose Rust to Lua as a module: `require("clock").now()`.
    let clock = rt.lua().create_table()?;
    clock.set("now", rt.lua().create_function(|_, ()| Ok(0))?)?;
    rt.register_module("clock", clock)?;

    // Chunks run as futures on the runtime's Tokio executor.
    let answer: i64 = rt.block_on(rt.eval("return 6 * 7", "=example"))?;
    rt.block_on(rt.exec(r#"print(require("serde").json.encode({ answer = 42 }))"#, "=example"))?;

    // Wait for tasks Lua spawned (`utils.spawn_task` and friends).
    rt.block_on(rt.wait_for_tasks())?;
    Ok(())
}

// A module can come from another crate, which implements `HostModule` for it.
struct Greeting;

impl HostModule for Greeting {
    fn name(&self) -> &str {
        "greeting"
    }

    fn load(&self, lua: &Lua) -> mlua::Result<Value> {
        let module = lua.create_table()?;
        module.set("hello", lua.create_function(|_, name: String| Ok(format!("hello, {name}")))?)?;
        Ok(Value::Table(module))
    }
}

// A store can load modules from anywhere, not just the filesystem.
struct SqliteStore { /* ... */ }

impl ModuleStore for SqliteStore {
    fn fetch(&self, name: &ModuleName) -> Result<Option<ModuleSource>, StoreError> {
        // SELECT source FROM modules WHERE name = ?
        Ok(None)
    }
}
```

## Sandbox limitations

- **PUC Lua, not Luau.** The sandbox is avarice's own work: it withholds libraries and wraps
  `load` and `setmetatable`, on a Lua that was not designed to run untrusted code. Luau was, and is
  far more battle-tested at it. [ADR 0001](docs/adr/0001-puc-lua-not-luau.md) says why this project
  uses PUC Lua.
- **The time limit is checked only between Lua instructions**, every 10,000 by default
  (`check_interval`). Anything else runs to its end before the limit can stop it:
  - **One call into Lua's C library.** Pattern matching backtracks, so
    `string.find(string.rep("a", 5000), "(.-)(.-)(.-)b")` runs for minutes under a one-second
    limit. A cancel doesn't stop it either.
  - **A synchronous Rust function**, such as a host module's, or a module store's `fetch` behind
    `require`. A cancel doesn't stop it either.
  - **An async Rust function.** Its time counts, but the wait isn't cut short: the chunk stops when
    it next runs Lua. A cancel does end the wait.
- **The memory cap counts only what Lua allocates.** Memory that Rust code allocates for itself,
  such as a host function's working buffers or what a userdata's value owns on the heap, is not
  counted and not capped.

## Limitations / WIP

- Needs `cargo test` testing on Windows.

> - PATHEXT lookup. a_program_is_found_on_the_path_the_command_gives runs avarice-copy by bare name, which should find avarice-copy.exe.
> - Windows-only code in process.rs: the reader for merged stdout and stderr, the host-stdout handle used when stderr goes to stdout, and the non-Unix branches for arguments and terminate.
> - Older Windows-only branches: env_value in utils.rs and upload_path in http/client/request.rs.
> - :kill(), :terminate(), and Ctrl-C with a Child running. The Ctrl-C tests are Unix-only, so try that one by hand.

## Licence

Apache License 2.0; see [LICENSE](LICENSE). The stdlib files taken from Astra are under the same
licence, and each has a header saying where it came from and what changed. `datetime`, `dirs` and
`process` are original to avarice, and their files say so instead.
