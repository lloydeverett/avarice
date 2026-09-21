# avarice-rt-stdlib

The **stdlib modules** of avarice-rt: `http`, `fs`, `crypto`, `serde`, `datetime`, `utils`,
`stores` and `validation`, as `mlua` values for the `avarice-rt` core to register.

Most of them are [Astra](https://github.com/ArkForgeLabs/Astra)'s, by ArkForge LLC, licensed under
the Apache License 2.0, kept as close to Astra's own files as they can be. This crate exists so
that the derived code and its licence obligations sit in one unit: the Apache-2.0 boundary is this
directory. The rest of the repository carries no licence, because it has no obligation to. (`ansi`,
which is original to avarice-rt, is in the core rather than here: it is a core module, not a stdlib
one, so `print` can use it in every runtime; see ADR 0011.)

The crate depends on `mlua` and never on `avarice-rt`, so the dependency runs one way only. It is
`publish = false`.

## Provenance

Everything under `src/components/` is Astra's, and so is everything under `lua/`, from version
0.51.2 (commit `885586cca0ef065ac80d6a7c702d05e60fbdbb47`), at the same relative path: Astra's
`astra/lua/` is `lua/` here. Each of those files opens with a header that names the Astra file, the
copyright holder and the licence, and has a `Changes from the original:` list:

- A file that is otherwise unchanged says `none`, and below its header it is byte-for-byte
  Astra's: `sed '1,/^$/d' <file>` gives Astra's file exactly.
- A file that differs lists every change. The changes are removals of what is not taken, the
  respelling of `mlua::SerializeOptions` for the mlua this workspace is on, and a few additions,
  such as a `__tostring` on each userdata, so that `print` can show which value one is (ADR 0006
  says why, and that a userdata is expected to have one). Nothing Astra does has been altered in
  how it works.
- `LICENSE` is the Apache License 2.0. Astra distributes no `NOTICE` file, so there is none here.
  Astra's own `LICENSE` differs from the canonical text in section 8 and in the appendix; this is
  the canonical text. The copyright line in the headers, `Copyright 2024 ArkForge LLC`, is the one
  Astra's `LICENSE` appendix carries.

`src/lib.rs` and `src/modules.rs` are this crate's own, not derived from an Astra file, and have no
header. `modules::load` makes the same
registration calls that Astra's `register_components` does, and loads the same Lua files.

**Not taken:** the HTTP server, templates, the database, Astra's own `require` (`import.rs`), and
the Lua layers `templates.lua`, `database.lua` and `test.lua`; the WebSocket client, which does not
satisfy mlua 0.12's `Sync` bound on userdata under the `send` feature; and Astra's `main.rs`,
`commands/` and `build.rs`.

## Features

Each module is behind a Cargo feature named `stdlib-<module>`, and `stdlib` turns on all eight; it
is this crate's default, and `avarice-rt` turns the defaults off and forwards the same names, so a
feature means the same on both. A module's dependencies are `optional`, behind its feature
([ADR 0007](../../docs/adr/0007-stdlib-modules-are-compile-time-optional.md)).

Astra's files are not split, so a module that needs another's Rust takes the whole file. Three
features that start with an underscore, `_astra_serde`, `_astra_utils` and `_astra_buffers`, are
not part of the API: they compile that file, and the dependencies it needs, without registering the
module it belongs to. `http` turns on `_astra_serde` (for `sanetize_lua_input`) and `_astra_buffers`
(for `AstraBuffer`), `fs` turns on `_astra_buffers`, and `validation` turns on `_astra_utils` (for
`AstraRegex`). Every gate is an attribute in `components/mod.rs`, which is the only Astra file that
changed for it, and its header lists them; no other file has a `#[cfg]` added.

`StdModule` has every variant in every build, and `StdModules::ALL` is the modules compiled in.
`loader` for one that is not returns an error naming the feature, and does not panic.

## How a module is built

Astra's own shape: a Rust half, `src/components/<module>.rs`, whose `register_to_lua(lua)` sets
primitives on the Lua globals as `astra_internal__<name>`, and a Lua file, `lua/<module>.lua`, that
wraps them into the module table and reads them off `_G` when called. `stores` has no Rust half at
all, and is **pure**: Lua only, computing over what it is given and reaching nothing outside the Lua
state, so it runs under every limit the runtime puts on Lua. That is why sandbox mode registers it,
and why giving it a Rust half would move it out of the sandbox (ADR 0007). The Lua source is
embedded with `include_str!`; there is no build script. One module is loaded differently, in
`src/modules.rs` and not in Astra's file: `validation.lua` defines `number`, `struct`, `regex` and a
dozen more as *global* functions, which would appear in every program's globals as soon as anything
required the module. It therefore runs against a table of its own that reads through to the real
globals. It also registers the regex primitive itself, which Astra's `utils` Rust half would
otherwise have to have set first.

`loader` maps a `StdModule` to a function that does both, on demand. Modules are registered
lazily, so nothing here runs until a script first requires the module: in particular the
`astra_internal__*` globals do not exist until then. They are an implementation detail, not an
interface — a script can see them and overwrite them.

## What the core has to accept

Astra runs tasks with `tokio::spawn`, which needs Lua to be `Send`, so this crate turns on mlua's
`send` feature and Cargo unifies it across the workspace. Everything `avarice-rt` hands to Lua
therefore has to be `Send`, and the core's shared state is `Arc`/`Mutex`. See ADR 0004's
amendment.
