---
status: ready-for-agent
adrs: [0022, 0023]
---

# `bridge`: Bridge tables and Bridge cells

## Problem Statement

An embedder, or the author of a **host module**, wants to follow a value a Lua script keeps: a
script's settings, its state, a list it builds up. Today the only way to learn what changed is to
read the whole table again and compare it with the last copy, which costs time in proportion to the
whole value rather than to the change, and cannot tell a key set to the same value from one never
touched. The `stores` module has observables, but it is Lua alone: nothing in Rust can subscribe to
one.

## Solution

A new **stdlib module**, `bridge`, gives Lua two values the host can subscribe to:

- A **Bridge table**, `bridge.table(init?)`, which a script writes and reads as it would a Lua
  table. Each write tells every subscriber which key changed, from what, to what.
- A **Bridge cell**, `bridge.cell(value?)`, which holds one value at a time, replaced whole. Each
  replacement tells every subscriber the old value and the new.

A script makes one and hands it to a host function, which subscribes from Rust. Lua can subscribe
too, and either side can write. Subscribers are called at once, inside the write, after it is in
place. Lua owns each one: the host's hold on it is weak, and the host is told once when it ends,
whether a side closed it or Lua let it go.

## User Stories

1. As a script author, I want to make a Bridge table with `bridge.table()`, so that the host can
   follow what I put in it.
2. As a script author, I want to make a Bridge table from an existing table with
   `bridge.table(init)`, so that I can start from values I already have.
3. As a script author, I want `bridge.table(init)` to copy `init`, so that my own later writes to
   `init` are not mistaken for writes to the Bridge table, and cannot bypass its subscribers.
4. As a script author, I want to write `t.key = value` and `t[key] = value`, so that a Bridge table
   reads like the Lua I already write.
5. As a script author, I want to read `t.key` and `t[key]`, so that reading needs nothing new.
6. As a script author, I want `#t` to give the length of a Bridge table's sequence, so that array
   code works on it.
7. As a script author, I want `pairs(t)` and `ipairs(t)` to walk a Bridge table, so that loops work
   on it.
8. As a script author, I want `table.insert`, `table.remove`, `table.concat`, `table.sort` and
   `table.unpack` to work on a Bridge table, so that the table library is still available.
9. As a script author, I want to store any Lua value in a Bridge table, functions and userdata
   included, so that I am not limited to plain data.
10. As a script author, I want a table stored in a Bridge table to be stored by reference, as in
    plain Lua, so that nothing is copied behind my back.
11. As a script author, I want to nest Bridge tables, so that I can have changes at every level
    tracked when I want it.
12. As a script author, I want every key of a Bridge table to be mine, `close` and `subscribe`
    included, so that a table whose keys come from outside, such as a JSON object, cannot collide
    with an operation.
13. As a script author, I want the operations on a Bridge table to be functions of `bridge.table`
    taking it first, such as `bridge.table.close(t)`, so that they never hide a key or are hidden
    by one.
14. As a script author, I want `bridge.table.insert(t, pos, value)` and
    `bridge.table.remove(t, pos)`, so that an insertion or removal in the middle of a sequence is
    told to subscribers as one list rather than one call per shifted slot.
15. As a script author, I want `bridge.table.batch(t, fn)`, so that many writes are told to
    subscribers as one list when `fn` returns.
16. As a script author, I want a batch inside a batch to fold into the outermost one, so that I can
    call a function that batches from inside my own batch.
17. As a script author, I want the writes a batch made before it raised to stay in place and be told
    before the error is raised again, so that subscribers never miss a change that happened.
18. As a script author, I want `bridge.table.snapshot(t)` to give me a plain shallow copy, so that
    I can hand the contents to code that wants a real table.
19. As a script author, I want `bridge.table.subscribe(t, fn)` to call `fn(changes)` with a list of
    `{ key, old, new }` entries, so that Lua code can react to changes too.
20. As a script author, I want a change's `new` to be nil when a key was removed, and `old` nil when
    a key was added, so that one shape covers adds, updates and removals.
21. As a script author, I want `subscribe` to return a handle with `unsubscribe()`, so that I can
    stop listening.
22. As a script author, I want a second function given when subscribing, `on_close`, to be called
    once when the object closes, so that my code can tidy up.
23. As a script author, I want `bridge.table.close(t)`, so that I can say I am finished and the
    host is told now rather than at some later collection.
24. As a script author, I want reading a closed Bridge table to still work and writing it to raise,
    so that a last look is possible and a late write is not lost silently.
25. As a script author, I want a Lua subscriber that raises not to fail my write, and its error to
    be reported on standard error, so that one bad subscriber neither breaks the writer nor hides.
26. As a script author, I want the other subscribers to be told even when one raises, so that a bad
    subscriber does not starve the rest.
27. As a script author, I want to make a Bridge cell with `bridge.cell(value?)`, starting at `value`
    or nil, so that I can share a single value.
28. As a script author, I want `c:get()` and `c:set(value)`, so that a Bridge cell is plain to use.
29. As a script author, I want `c:update(fn)` to set `fn(old)`, so that read-modify-write is one
    call.
30. As a script author, I want every `set` to be told, even of a value equal to the old one, so that
    I can set a table I changed in place to say it changed.
31. As a script author, I want `c:subscribe(fn, on_close?)` to call `fn(old, new)`, so that a cell's
    subscriber is not handed a list with a meaningless key.
32. As a script author, I want `c:close()` and the same closing rules as a Bridge table, so that the
    two behave alike.
33. As a script author, I want `print(t)` to show a Bridge table's contents marked
    `bridge.table { ... }`, and `print(c)` to show `bridge.cell(...)`, so that I can see what it
    holds and that it is not a plain value.
34. As a script author, I want `serde` to encode a Bridge table as its contents and a Bridge cell as
    its value, so that I can write one out as JSON or another format.
35. As a script author, I want nested Bridge tables to encode all the way down, so that what is
    written out is the whole value.
36. As a script author, I want `require("bridge")` to fail in a runtime that does not register it,
    as any missing module does, so that I find out at once.
37. As a host module author, I want a host function to receive a Bridge table as a `bridge::Table`
    and a Bridge cell as a `bridge::Cell`, so that I can accept one from a script with the usual
    `FromLua` conversion.
38. As a host module author, I want to subscribe from Rust with a callback taking `&Lua` and a slice
    of `bridge::Change`, so that I am told exactly what changed.
39. As a host module author, I want each change's key, old value and new value as `mlua::Value`s, so
    that I can borrow my own userdata back, call a function, or convert with `FromLua`.
40. As a host module author, I want a Rust subscription to be a guard that unsubscribes when
    dropped, so that I cannot leak one by forgetting to unsubscribe.
41. As a host module author, I want a close callback, called once with no `Lua`, so that I learn
    when the object is done even if it ends during collection or while the runtime is dropped.
42. As a host module author, I want my hold on a Bridge table not to keep it alive, so that a script
    that forgets one does not leave it held for ever.
43. As a host module author, I want to read, write and close a Bridge table or cell from Rust, so
    that the host can change it too.
44. As a host module author, I want my writes to tell Lua's subscribers at once, so that a script
    reacts to the host as the host reacts to it.
45. As a host module author, I want writes to a closed object to return an error rather than panic,
    so that I can handle the race between Lua closing it and me writing.
46. As a host module author, I want my callback to return nothing, so that the contract says plainly
    that my errors are mine to handle.
47. As a host module author, I want it documented that my callback runs on the Lua thread inside the
    write and should hand off slow work, so that I do not stall the script by accident.
48. As a host module author, I want my callback to be allowed to write to the object it is
    subscribed to, so that I can, for example, normalise a value as it arrives.
49. As a host module author, I want a subscriber added during a notification to hear only later
    changes, and one removed during it to stop at once, so that the order of events is predictable.
50. As an embedder, I want `bridge` behind its own Cargo feature, `stdlib-bridge`, so that a build
    that does not want it leaves it out.
51. As an embedder, I want trusted mode to register `bridge` when it is compiled in, and sandbox
    mode not to, as for every module with Rust behind it, so that the profiles keep their rules.
52. As an embedder, I want to add `bridge` to a sandboxed runtime with `with_std_modules`, so that a
    plugin I do not vouch for can still talk to my host through it.
53. As an embedder, I want `StdModules` and `StdModule` to name `bridge` in every build, so that the
    API does not change with features.
54. As a host module author, I want any userdata of mine whose metatable has `__view` and `__name`
    to be shown by `print` through `__view`, so that my own data-like userdata can print well too.
55. As a script author, I want a table with a `__view` field to print as it does today, so that a
    script cannot make `print` run its code.
56. As a script author, I want a `__view` that raises to leave `print` showing the value as it would
    without one, so that printing never fails because of it.
57. As a script author, I want a view that leads back to the value it came from to print as a cycle,
    so that printing it terminates.

## Implementation Decisions

- **The module.** `bridge` is a stdlib module with Rust behind it, original to avarice. It is
  compiled in by `stdlib-bridge`, joins `StdModule`/`StdModules` in every build, and is registered
  by trusted mode only. It is added to the feature matrix's list of modules and to the README's
  Stdlib section.
- **Two object types**, exposed to Rust in a public `bridge` module as `bridge::Table`,
  `bridge::Cell` and `bridge::Change`, always named through the module path. Each Lua object is a
  userdata that is a thin handle on shared state; the Rust types are handles on the same state, and
  convert from Lua with `FromLua`.
- **Backing storage.** A Bridge table's contents live in an ordinary Lua table, its backing table,
  held from the shared state; a Bridge cell's value is held the same way. Values are any Lua value,
  stored as they are, and tracked shallowly. `bridge.table(init)` copies `init` shallowly into a new
  backing table; nothing is told, since nothing can be subscribed yet.
- **Lua surface of a Bridge table.** Metamethods `__index`, `__newindex`, `__len`, `__pairs`,
  `__tostring`, `__name` (`bridge.table`) and `__view`, and no methods: `__index` reads only the
  backing table, so every key is the script's. `bridge.table` is a table with a `__call`, so
  `bridge.table(init?)` makes one, and its fields are the operations, each taking the Bridge table
  first and raising if given anything else: `subscribe(t, fn, on_close?)`, `batch(t, fn)`,
  `insert(t, pos, value)`, `remove(t, pos)`, `snapshot(t)` and `close(t)`.
- **Lua surface of a Bridge cell.** Methods `get()`, `set(value)`, `update(fn)`,
  `subscribe(fn, on_close?)` and `close()`, with `__tostring`, `__name` (`bridge.cell`) and
  `__view`. No property syntax. A cell has no keys for a method to hide, so it keeps methods.
  `bridge.cell` is a function.
- **The change contract.** A table subscriber in Rust is `Fn(&Lua, &[bridge::Change])` returning
  nothing, where a `Change` has `key`, `old` and `new` as `mlua::Value`, nil standing for absent. A
  cell subscriber in Rust is `Fn(&Lua, old, new)`. Close callbacks take no `Lua`. Rust's `subscribe`
  returns a guard that unsubscribes on drop. Subscribing from Lua returns a handle with
  `unsubscribe()`. Lua table subscribers get an array of `{ key, old, new }` tables.
- **When subscribers run.** Synchronously, on the Lua thread, inside the write, after the write is
  applied; every subscriber, the writer's own side included. A write from Rust runs Lua subscribers
  inside that call. The subscriber list is copied, or otherwise not locked, while subscribers run,
  so a subscriber can write, subscribe or unsubscribe; writes nest, a subscriber added mid-way hears
  only later changes, and one removed mid-way is not called again.
- **Batching.** A per-object batch depth and a pending list. `batch(t, fn)` raises the depth, runs
  `fn` under a protected call, lowers the depth, and at the outermost level tells the pending list
  (if non-empty) in write order, uncoalesced, then raises `fn`'s error again if it had one.
  `insert` and `remove` are batches that shift the sequence themselves. Writes through any path,
  the `table` library included, join a batch that is open.
- **Errors.** A Rust subscriber's panic propagates. A Lua subscriber's error is written to standard
  error, in the manner of Astra's task errors, and the next subscriber is called; nothing is raised
  at the write.
- **Lifetime.** The host's handles and subscriptions do not keep the object alive. Closing from
  either side, or the Lua userdata being dropped, closes it: every close callback runs once, writes
  then raise in Lua and return an error in Rust, and reads still work while the state lives. The
  drop is the userdata's Rust `Drop`, not a Lua finalizer, so ADR 0018 does not apply.
- **`serde`.** The userdata is made with `create_ser_userdata`, and the shared state implements
  `serde::Serialize` as its backing table (or the cell's value). Astra's `serde` files are not
  changed.
- **`print` and `__view`.** `print` gains one rule: a userdata whose metatable has a function
  `__view` and a `__name` is shown as the `__name` followed by `__view(value)`'s result: laid out as
  a table after a space when it is one, otherwise in parentheses. A `__view` that raises falls back
  to today's output. Cycles through views are caught by the existing cycle check. A table's
  `__view` is ignored.
- **Docs.** ADR 0022 (`bridge`) and ADR 0023 (`__view`) record the decisions; `CONTEXT.md` has
  **Bridge table** and **Bridge cell**. The README gets a `bridge` section, including the Rust side
  and the note that subscribers must return quickly.

## Testing Decisions

- **What a good test is here.** Each test drives a runtime through its public API, as an embedder
  does, and checks what a script or a host function observes: what a subscriber was given, in what
  order, what was raised, what was printed. No test reaches into the shared state or counts
  internal calls.
- **The seam: Lua through a trusted `Runtime`.** A new integration test suite for `bridge`, gated
  on `stdlib-bridge`, with `eval` and `raises` helpers as the `tidings` suite has. The Rust side is
  tested through the same seam: the test registers a host module of its own, whose functions take a
  `bridge::Table` or `bridge::Cell`, subscribe with a callback that records what it is given into
  shared state the test then reads, and can write or close from Rust. Covered:
  - reads, writes, length, iteration and the `table` library on a Bridge table;
  - keys named like the operations (`close`, `subscribe`, ...) read and write as any key;
  - the operations raising when given something other than a Bridge table;
  - the list a single write gives, from Lua and from Rust subscribers;
  - adds, updates and removals in the change shape;
  - `batch`: one list, write order, folding, writes kept and told when `fn` raises;
  - `insert` and `remove` as one list, and `table.insert` at the front as many;
  - re-entrant writes, and subscribing and unsubscribing during a notification;
  - a Lua subscriber that raises: write succeeds, the others are told;
  - `close` from Lua and from Rust: close callbacks once, writes refused, reads allowed;
  - collection closing an object when Lua drops it, with a Rust hold not preventing it;
  - a Rust subscription guard unsubscribing when dropped;
  - `bridge.table(init)` copying, `snapshot` copying;
  - a Bridge cell's `get`, `set`, `update`, telling every `set`, and `(old, new)`;
  - `serde` encoding a Bridge table, a nested one and a cell, gated on `stdlib-serde` as well.
- **The `print` suite** checks `__view` with its buffer write sink: a userdata the test makes with a
  `__view` and `__name` prints through it; a table with a `__view` field prints as before; a
  `__view` that raises falls back; a view cycle prints as a cycle. These need no feature. Bridge
  tables and cells printing with their markers are checked there too, gated on `stdlib-bridge`.
- **The compiled-in and feature-matrix checks** gain `bridge`: the compiled-in suite's list of
  modules, the features suite, the sandbox suite's check that sandbox mode registers only pure
  modules, and the matrix script's list, so `bridge` alone builds and passes.
- **Prior art.** The `tidings` suite for a stdlib module driven from Lua, the contributed-modules
  suite for a test's own host module, and the `print` suite for output through the write sink.

## Out of Scope

- Channels, or any value without keys that carries messages between the sides.
- Host-created Bridge tables or cells, and any registry of names for them.
- A strong hold for the host that keeps one alive after Lua lets it go.
- Change feeds, Resyncs, bounded buffers, or any delivery off the Lua thread.
- Tracking changes inside plain tables stored in a Bridge table.
- Coalescing changes to the same key within a batch.
- Event kinds such as "inserted" alongside `(key, old, new)`.
- A hook for where Lua subscriber errors are reported.
- `serde` decoding into Bridge tables.
- Registering `bridge` in sandbox mode by default.

## Further Notes

- Subscriber errors are reported the way Astra's task errors are. If an embedder later asks to
  capture them, one hook on the builder could cover both.
- `__view` is general: any host module whose userdata stands for data can give one, and `print`
  needs no further change for it.
- A Rust subscriber that keeps an `mlua::Value` of its own Bridge table keeps it alive. The README
  says so.
