# avarice-rt-stdlib

The **stdlib modules** of avarice-rt: `http`, `fs`, `crypto`, `serde`, `datetime`, `utils`,
`stores` and `validation`, as `mlua` values for the `avarice-rt` core to register.

They are [Astra](https://github.com/ArkForgeLabs/Astra)'s, by ArkForge LLC, licensed under the
Apache License 2.0, kept as close to Astra's own files as they can be. This crate exists so that
the derived code and its licence obligations sit in one unit: the Apache-2.0 boundary is this
directory. The rest of the repository carries no licence, because it has no obligation to.

The crate depends on `mlua` and never on `avarice-rt`, so the dependency runs one way only. It is
`publish = false`.

## Provenance

[`UPSTREAM.md`](UPSTREAM.md) lists every file taken from Astra, which of them are identical to
Astra's, which had to change, and what was left out. The short version:

- A file that is byte-for-byte Astra's has **no header**, so it stays that way.
- A file that differs opens with a header naming the Astra file it came from and listing what
  changed. The list is never empty and is limited to removals and the respelling of an mlua path.
- `LICENSE` is the Apache License 2.0. `NOTICE` is ours: Astra distributes no `NOTICE` file, so
  none is inherited, and this one names ArkForge LLC as the source.

`src/lib.rs` and `src/modules.rs` are not derived and have no header.

## How a module is built

Astra's own shape: a Rust half, `src/components/<module>.rs`, whose `register_to_lua(lua)` sets
primitives on the Lua globals as `astra_internal__<name>`, and a Lua file, `lua/<module>.lua`,
that wraps them into the module table and reads them off `_G` when called. `stores` has no Rust
half. The Lua source is embedded with `include_str!`; there is no build script. One module is
loaded differently: `validation.lua` defines its functions as globals, so it runs against a table
of its own, and registers the regex primitive itself. `UPSTREAM.md` says why.

`loader` maps a `StdModule` to a function that does both, on demand. Modules are registered
lazily, so nothing here runs until a script first requires the module: in particular the
`astra_internal__*` globals do not exist until then. They are an implementation detail, not an
interface — a script can see them and overwrite them.

## What the core has to accept

Astra runs tasks with `tokio::spawn`, which needs Lua to be `Send`, so this crate turns on mlua's
`send` feature and Cargo unifies it across the workspace. Everything `avarice-rt` hands to Lua
therefore has to be `Send`, and the core's shared state is `Arc`/`Mutex`. See ADR 0004's
amendment.
