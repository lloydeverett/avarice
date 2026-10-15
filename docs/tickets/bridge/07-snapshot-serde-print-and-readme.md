# 07: Seeing what a Bridge value holds: snapshot, `serde`, `print`, README

**What to build:** `bridge.table.snapshot(t)` returns a plain shallow copy. `serde` encodes a Bridge
table as its contents and a Bridge cell as its value, nested ones included, through serializable
userdata, with Astra's `serde` files unchanged. Both give a `__view`, so `print` shows
`bridge.table { ... }` and `bridge.cell(...)`. The README gets a `bridge` section covering Lua and
Rust use, that Rust subscribers must return quickly, and that keeping an `mlua::Value` of a Bridge
table keeps it alive.

A Bridge table's backing table is a user value of its userdata (see the header of
`src/stdlib/components/bridge/mod.rs`), and `Serialize` on the Rust state cannot reach a user value
directly. Prototype getting at the backing table, for example through a `WeakLua` and the id lookup
ticket 03 gives the host's handle, before committing to `create_ser_userdata`.

**Blocked by:** 01, 04, 06

**Status:** ready-for-agent

- [ ] `snapshot` returns a plain table; writing to it tells no one
- [ ] JSON encoding of a Bridge table, a nested one and a cell gives the plain data (tests gated on
      `stdlib-serde` too)
- [ ] `print` shows both with their markers (in the `print` suite, gated on `stdlib-bridge`)
- [ ] README `bridge` section, and `bridge` in the Stdlib module list
