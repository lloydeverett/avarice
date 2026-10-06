---
status: accepted
---

# `dirs` resolves standard per-application directories, via `etcetera`

`dirs` is a new **stdlib module**: it resolves where an application's config, data, cache, state
and runtime directories belong, on the host it is running on. It is original to avarice, the
second stdlib module (after `ansi`, later moved to the core by
[ADR 0011](0011-print-highlights-and-ansi-is-a-core-module.md)) that owes nothing to
[Astra](https://github.com/ArkForgeLabs/Astra), and the first that both has a Rust half and is not
Astra's.

A reader would otherwise wonder why `src/stdlib` suddenly depends on a crate Astra never used, why
one module's Lua and Rust files carry no `Changes from the original:` header when every neighbour
does, and why `stores` — the module named for storing things — has nothing to do with it.

## The decision

- **The name is `dirs`, not `stores`.** `stores` already names a **stdlib module**: Astra's
  `Observable`/`PubSub` pair, **pure** Lua with no filesystem or environment access at all. A
  module that resolves filesystem paths from the host's environment cannot be pure and cannot
  share that name without contradicting what it already means in this codebase.
- **It wraps [`etcetera`](https://docs.rs/etcetera) 0.11**, a new optional dependency behind a new
  `stdlib-dirs` feature, following the one-feature-per-module pattern
  ([ADR 0007](0007-stdlib-modules-are-compile-time-optional.md)). `etcetera` only computes paths;
  it does no I/O itself, which `dirs` inherits (see below).
- **It resolves the whole family, not just config**: `config`, `data`, `cache`, `state` and
  `runtime`, mirroring `etcetera`'s `AppStrategy` trait. The request that started this was for
  config directories specifically, but the other four cost nothing extra once `etcetera` is a
  dependency, and splitting them into a later module would have meant a second, near-identical
  original module.
- **The call shape is identity-once.** `dirs.app(app_name, author, top_level_domain)` returns a
  value with `:config()`, `:data()`, `:cache()`, `:state()`, `:runtime()` methods, rather than
  three positional strings repeated on every call. `etcetera`'s `AppStrategyArgs` needs all three
  regardless of platform — `Xdg` (Linux and macOS, `choose_app_strategy`'s default there) ignores
  `author` and `top_level_domain`, but `Windows`'s strategy uses `author` to place things under
  `%APPDATA%\{author}\{app_name}`. Requiring all three from Lua, once, is what makes Windows work
  properly rather than as an afterthought; repeating them on every one of five calls would only
  invite a mismatch between calls that were meant to describe the same application.
- **`config()`, `data()` and `cache()` always return a string. `state()` and `runtime()` can
  return `nil`.** This corrects the design conversation that preceded this ADR, which assumed only
  `runtime()` was optional: `etcetera`'s `AppStrategy::state_dir()` is `Option<PathBuf>` too — some
  strategies (`Apple`, `Windows`) do not support a state directory at all, and even `Xdg`'s
  can come back `None` if `$XDG_STATE_HOME` holds a relative path, which the XDG spec says to
  reject. `runtime()` is `nil` under the same rule, most commonly because `$XDG_RUNTIME_DIR` is
  unset. Neither is an error: a script that cares checks with `if`.
- **No I/O, ever.** A method only computes a path string; nothing is created, and nothing is read
  or written. Creating the directory before writing into it is `fs.create_dir_all`'s job, on the
  path `dirs` hands back — `dirs` does not duplicate any part of what the existing `fs` stdlib
  module already does.
- **Not pure, so trusted-mode-only.** Resolving a path means reading environment variables
  (`$XDG_CONFIG_HOME` and siblings) and the OS's notion of the user's home directory, which needs
  Rust; a pure, Lua-only implementation was not on the table once the request named `etcetera`
  specifically. Per [ADR 0007](0007-stdlib-modules-are-compile-time-optional.md), that means
  sandbox mode never registers `dirs`, the same as `fs`, `http` and every other module with a Rust
  half. Giving sandboxed Lua scoped disk access at all is a separate, larger design question this
  does not attempt.
- **Original, so no Astra header, no `Changes from the original:` list.** `src/stdlib/components/dirs.rs`
  and `src/stdlib/lua/dirs.lua` each open with one line saying they are original to avarice.
  `src/stdlib/components/mod.rs`, which is Astra's and derived, gains one line,
  `pub mod dirs;` behind `stdlib-dirs`, recorded in its header as an addition under the rule
  [ADR 0006](0006-stdlib-derived-from-astra.md)'s amendments allow: nothing Astra does is altered.
- **One Rust primitive per directory kind**, not one primitive dispatching on a "kind" string:
  `astra_internal__dirs_config`, `_data`, `_cache`, `_state`, `_runtime`, each taking
  `(app_name, author, top_level_domain)`. This follows the shape Astra's own `utils.rs` uses for
  unrelated single-purpose primitives (`astra_internal__getenv`, `astra_internal__uuid`) rather
  than inventing a dispatch convention with no precedent in this codebase. `dirs.lua`'s `App` is a
  plain Lua table with a metatable, not a Rust userdata: there is no state to hold between calls
  beyond the three strings Lua already has, so a userdata would add machinery without adding
  capability.

## Considered options

**Config only**, matching the literal request, was considered and rejected once `etcetera` was
confirmed as the dependency: the other four directory kinds are the same `AppStrategy` call with a
different method name, so leaving them out would not have kept the module smaller in any way that
mattered, only made it incomplete.

**Bundling `read`/`write` convenience methods on `dirs` itself** was rejected. `fs` already has a
general path-based file API; a second, narrower one on `dirs` would duplicate part of it for no
benefit and give the two modules two chances to drift apart.

**Auto-creating the directory when resolved** was rejected, to keep `dirs` doing exactly one thing
— resolution — and to match `etcetera`'s own no-I/O design. A script that wants the directory to
exist calls `fs.create_dir_all` on the path `dirs` gives it.

**A single Windows-placeholder `author`/`top_level_domain`, supplied by avarice rather than the
caller** was considered, since Linux and macOS ignore both fields under `Xdg`. It was rejected
because it would have made Windows support cosmetic rather than real: every application would land
under the same placeholder author, defeating the reason `AppStrategyArgs` has the field at all.

## Consequences

**The count is nine.** `stdlib` now turns on nine features, `StdModules` gains a tenth flag
(`DIRS`, `1 << 8`, still inside the existing `u16` — no width change, unlike
[ADR 0008](0008-ansi-is-original-and-pure.md)'s), and `scripts/check-features.sh` builds eleven
combinations: no modules, all of them, and each of the nine alone. Tests that enumerate module
names or bit values (`src/stdlib/modules.rs`, `tests/compiled_in.rs`, `tests/stdlib.rs`)
list `dirs` alongside the other eight; tests that derive their expectations from `StdModules::all()`
or `StdModule::ALL` needed no change.

**`stores` is unchanged.** It keeps its name, its meaning, and its place as the one Astra-derived
pure module; this decision only means a second, unrelated pure-Lua-vs-Rust-half distinction now
exists in the same file (`stores` pure and Astra's, `dirs` impure and original), which the existing
`pure` field on each module's table entry already expressed correctly without needing a new axis.

**A caller on Windows must supply a real `author` and `top_level_domain`, not placeholders, to get
a well-organized path.** A script that only cares about Linux and macOS can pass anything for
those two fields — `Xdg` never reads them — but is still required to pass something, which is a
small tax on the common case paid for Windows's sake.
