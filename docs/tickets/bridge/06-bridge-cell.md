# 06: Bridge cell

**What to build:** `bridge.cell(value?)` makes a **Bridge cell** holding one value, replaced whole,
with methods `get()`, `set(value)`, `update(fn)`, `subscribe(fn, on_close?)` and `close()`.
Subscribers get `(old, new)` (Rust: `Fn(&Lua, old, new)`) on every `set`, even of an equal value.
Rust receives it as `bridge::Cell`, can get, set, subscribe and close it, and its hold is weak, as
for a Bridge table.

**Blocked by:** 05

**Status:** ready-for-agent

- [ ] `bridge.cell()` starts at nil, `bridge.cell(v)` at `v`; `get`/`set`/`update` work
- [ ] Every `set`, including of a `rawequal` value, tells Lua and Rust subscribers `(old, new)`
- [ ] Lua subscriber errors, re-entrancy and unsubscribing behave as for a Bridge table
- [ ] Closing and collection behave as for a Bridge table
- [ ] `__tostring` gives `bridge.cell: 0x...`
