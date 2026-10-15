# 05: Closing a Bridge table, and Lua owning it

**What to build:** `bridge.table.close(t)` from Lua, or `close` from Rust, ends a Bridge table: each
subscriber's close callback runs once (Lua: the second function given to `subscribe`; Rust: a
callback with no `Lua`), writes then raise in Lua and return an error in Rust, and reads still work.
The host's `bridge::Table` and subscriptions are weak: when Lua drops its last reference, collection
closes the table the same way, through the userdata's Rust `Drop`. Collection tells only the Rust
close callbacks: a Lua one is collected with the table (ADR 0022).

**Blocked by:** 03

**Status:** ready-for-agent

- [ ] Closing from Lua and from Rust each run every close callback exactly once; a second close is
      harmless
- [ ] After close, writes raise in Lua and return an error in Rust; reads and `pairs` work
- [ ] A table Lua no longer references is closed by a full collection, even while Rust holds a
      `bridge::Table` and a subscription; its Rust close callbacks run, and no Lua one is called
- [ ] A Bridge table that refers to itself, or is captured by its own Lua subscriber, is collected
      and closed
- [ ] Dropping the runtime with an open table runs close callbacks without panicking
