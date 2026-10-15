# 07: Seeing what a Bridge value holds: snapshot, `serde`, `print`, README

**What to build:** `bridge.table.snapshot(t)` returns a plain shallow copy. `serde` encodes a Bridge
table as its contents and a Bridge cell as its value, nested ones included, through serializable
userdata, with Astra's `serde` files unchanged. Both give a `__view`, so `print` shows
`bridge.table { ... }` and `bridge.cell(...)`. The README gets a `bridge` section covering Lua and
Rust use, that Rust subscribers must return quickly, and that keeping an `mlua::Value` of a Bridge
table keeps it alive.

**Blocked by:** 01, 04, 06

**Status:** ready-for-agent

- [ ] `snapshot` returns a plain table; writing to it tells no one
- [ ] JSON encoding of a Bridge table, a nested one and a cell gives the plain data (tests gated on
      `stdlib-serde` too)
- [ ] `print` shows both with their markers (in the `print` suite, gated on `stdlib-bridge`)
- [ ] README `bridge` section, and `bridge` in the Stdlib module list
