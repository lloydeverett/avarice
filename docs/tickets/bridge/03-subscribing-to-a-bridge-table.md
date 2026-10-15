# 03: Subscribing to a Bridge table from Lua and Rust

**What to build:** Both sides can subscribe to a Bridge table and are told each change as a list of
`(key, old, new)`, synchronously, after the write. Lua uses `bridge.table.subscribe(t, fn)`, getting
an array of `{ key, old, new }` and a handle with `unsubscribe()`. Rust receives the table in a host
function as `bridge::Table` (via `FromLua`), subscribes with `Fn(&Lua, &[bridge::Change])`, gets a
guard that unsubscribes on drop, and can read and write the table, which tells Lua's subscribers.

Where things live (see the header of `src/stdlib/components/bridge.rs`): Lua subscriber functions
are stored in a user value of the Bridge table's userdata, as the backing table is, and not in Rust
state, so that a subscriber that captures its own table can still be collected. The host's
`bridge::Table` holds the object weakly, for example as an id it looks up in a weak-valued table
through a `WeakLua`, and never as an `AnyUserData` or `mlua::Table`, which are registry references
and so keep it alive.

**Blocked by:** 02

**Status:** done

- [x] One write tells every subscriber, the writer's side included, a list of one change; adds have
      nil `old`, removals nil `new`
- [x] A test's own host module receives a `bridge::Table`, subscribes, and records what it is told
- [x] A write from Rust tells Lua subscribers inside that call
- [x] A subscriber may write to the same table (nested notification); one added mid-notification
      hears only later changes; one removed mid-notification is not called again
- [x] A Lua subscriber that raises: the write succeeds, the error goes to stderr, the remaining
      subscribers are told
- [x] `bridge.table.subscribe` (and every `bridge.table.*` operation) raises when given something
      other than a Bridge table
- [x] Dropping the Rust guard, or calling `unsubscribe()`, stops notifications
