# 03: Subscribing to a Bridge table from Lua and Rust

**What to build:** Both sides can subscribe to a Bridge table and are told each change as a list of
`(key, old, new)`, synchronously, after the write. Lua uses `bridge.table.subscribe(t, fn)`, getting
an array of `{ key, old, new }` and a handle with `unsubscribe()`. Rust receives the table in a host
function as `bridge::Table` (via `FromLua`), subscribes with `Fn(&Lua, &[bridge::Change])`, gets a
guard that unsubscribes on drop, and can read and write the table, which tells Lua's subscribers.

**Blocked by:** 02

**Status:** ready-for-agent

- [ ] One write tells every subscriber, the writer's side included, a list of one change; adds have
      nil `old`, removals nil `new`
- [ ] A test's own host module receives a `bridge::Table`, subscribes, and records what it is told
- [ ] A write from Rust tells Lua subscribers inside that call
- [ ] A subscriber may write to the same table (nested notification); one added mid-notification
      hears only later changes; one removed mid-notification is not called again
- [ ] A Lua subscriber that raises: the write succeeds, the error goes to stderr, the remaining
      subscribers are told
- [ ] `bridge.table.subscribe` (and every `bridge.table.*` operation) raises when given something
      other than a Bridge table
- [ ] Dropping the Rust guard, or calling `unsubscribe()`, stops notifications
