---
status: accepted
---

# `bridge` lets the host watch Lua values change

A **stdlib module**, `require("bridge")`, compiled in by one feature, `stdlib-bridge`, gives Lua two
objects that Rust code can subscribe to: a **Bridge table**, made by `bridge.table(init?)`, and a
**Bridge cell**, made by `bridge.cell(value?)`. A subscriber is told of each change as it happens,
so a host that wants to follow a Lua value never diffs it to find out what moved. The module is
original to avarice and outside [ADR 0006](0006-stdlib-derived-from-astra.md)'s scope. It has Rust
behind it, so it is not **pure**, and sandbox mode does not register it.

A reader would otherwise wonder why a subscriber is called while the script that wrote is still
running, why the host cannot keep one alive, and why the module has no channel.

## What Lua sees

A Bridge table is userdata with `__index`, `__newindex`, `__len` and `__pairs`, so `t.x = 1`,
`t[k]`, `#t`, `pairs`, `ipairs` and the `table` library all work on it. `next`, `rawget`, `rawset`
and `type` see the userdata. Behind it is an ordinary Lua table, its backing table.

A Bridge table has no methods. Every key is the script's, `close` and `subscribe` among them, so
whatever is done to one besides reading and writing it is done by a function of `bridge.table`,
which is called to make one and indexed for the rest: `bridge.table.subscribe(t, fn, on_close?)`,
`batch`, `insert`, `remove`, `snapshot` and `close`, each taking the Bridge table first. A Bridge
cell has no keys for a method to hide, so its operations are methods.

It holds any Lua value, and tracks it shallowly: a plain table stored in it is stored by
reference, and a write into that table is not a change to the Bridge table. Tracking at every level
is done by nesting Bridge tables. `bridge.table(init)` copies `init`, shallowly, into a new backing
table, so that a reference to `init` kept elsewhere cannot write past the subscribers.

A Bridge cell holds one value, replaced whole: `get`, `set(value)`, and `update(fn)`, which sets
`fn(old)`. Every `set` is a change, even to a value `rawequal` to the old one, so a script can set
a table it has changed in place to say so.

`bridge.table.snapshot(t)` gives a plain shallow copy of a Bridge table. `serde` encodes a Bridge
table as its backing table and a Bridge cell as its value, because their userdata is made with
`create_ser_userdata`; Astra's `serde` is not changed. `print` shows both through the protocol of
[ADR 0023](0023-print-shows-userdata-through-view.md).

## How a change is told

**Synchronously, after the write.** A subscriber runs on the Lua thread, inside the write that
caused the change, once the write is in place, so it reads the new state. A Rust subscriber that
has slow work to do hands it off, to a task or a channel of its own, and returns. This is
documented, not enforced.

**As a list.** A Bridge table subscriber is given a list of changes, each a key, its old value and
its new one, with nil for a key that was not there or is no longer. In Rust that is
`Fn(&Lua, &[bridge::Change])`; in Lua, `fn(changes)`, where each change is
`{ key = k, old = o, new = n }`. One write is a list of one. `bridge.table.batch(t, fn)` gathers
every write made to `t` during `fn`, however it is made, into one list in the order they were made,
with no coalescing; batches inside it fold into the outermost; and if `fn` raises, the writes
already made stay, are told, and the error is raised again. `fn` cannot yield: a suspended batch
would hold back every other writer's changes to `t` until it resumed.
`bridge.table.insert(t, pos, value)` and `bridge.table.remove(t, pos)` are batches, so the shifting
they do is one list. A Bridge cell subscriber is given `(old, new)`, since a cell has no keys to
list.

**To everyone.** Every subscriber is told of every change, the writer's own included, from Lua or
from Rust. A write from Rust tells Lua's subscribers inside that call, holding the Lua lock, as any
Rust call into Lua does with `send` on. A subscriber may write in turn: that is a nested call, so
the inner change is told to everyone before the outer one reaches its next subscriber. A
subscriber added while a change is being told hears the next one; one removed stops hearing at
once.

**Without failing the write.** A Rust subscriber returns nothing: its errors are its own. One that
panics panics, as a panic in any host function does. A Lua subscriber that raises has its error
written to standard error, as Astra's tasks have theirs, and the next subscriber is told. The
write itself has succeeded in either case, and nothing is raised at it.

## Who owns one

Lua makes every Bridge table and Bridge cell, and hands it to the host through the host's own
functions, which receive it as a `bridge::Table` or a `bridge::Cell`. Lua owns it:

- **The host's hold is weak.** Neither a `bridge::Table` nor a subscription keeps the object
  alive.
- **It ends by being closed.** Closing it from either side ends it, `bridge.table.close(t)` or
  `c:close()` in Lua, and so does collecting it once Lua holds no reference. Every subscriber is
  then told once that it has closed: subscribing from Lua takes a second function, and a Rust
  subscription a second callback, given no `Lua`, since collection and dropping the runtime are
  where it may run. Collection tells only the Rust callbacks: a Lua one is being collected with
  the object, and nothing is left in Lua to hear it.
- **A closed one can be read and not written.** A write raises in Lua and returns an error in
  Rust.
- **Subscribing gives a way to stop.** Subscribing from Lua returns a handle with `unsubscribe`;
  from Rust, a guard that unsubscribes when dropped.

Collection closes it through the userdata's Rust `Drop`, which is the host's, so the refusal of
**Lua finalizers** in [ADR 0018](0018-the-sandbox-refuses-lua-finalizers.md) does not reach it.

## Considered options

**Data only, copied in**, refusing functions and userdata and copying a stored table into a
tracked one, was the first recommendation. It let the host keep a copy it could read off the Lua
thread. Synchronous subscribers have no such copy to keep, so the restriction bought nothing, and
an `mlua::Value` is more use to the host: it can borrow its own userdata back, call a function,
or convert with `FromLua`.

**Methods on a Bridge table**, `t:close()` and the like, were rejected. A method is found by the
same `__index` as a key, so a script's key `close` would either hide the method or be hidden by
it, and a table whose keys come from outside, a JSON object's or a user's, cannot promise to avoid
six names. Raising on a write to one of them was rejected for the same reason.

**A Change feed**, each subscriber reading changes from a bounded stream at its own pace and given a
Resync when it falls behind, as tidings does, was rejected for direct calls. A feed never makes Lua
wait on the host, but its subscriber learns of a change only after the script has moved on, and
needs a copy of each value to read it off the thread. A subscriber that wants a feed can fill one.
**Telling only which keys changed**, the subscriber reading their values when it likes, loses the
values between, and was rejected for the same reasons.

**A Rust subscriber that returns an error**, raised at the write, was rejected. The script that
wrote did nothing wrong, and has no way to know what the host's error means or to recover from it.
**Raising a Lua subscriber's error at the write**, at once or once everyone is told, was rejected
for the same reason; the error is still reported, not swallowed.

**Objects the host makes**, or a registry of names where host and Lua both open `"settings"`, were
rejected. Lua making them and passing them to a host function needs no names, which could collide,
and makes Lua's references the only measure of whether one is in use.

**A strong hold for the host** was rejected for now. Lua makes every one, so the host is never the
one that knows it is still wanted, and a Lua producer that forgot one would leave it held for ever.
Nobody has asked to keep one alive.

**A channel**, a third object without keys, carrying values from one side to the other, was
designed and then dropped. Nothing yet needs it that a Bridge cell's subscriber cannot do.

**A core module**, in every runtime like `ansi`, or a stdlib module the sandbox registers, was
rejected. `bridge` reaches nothing but the host, which suits the sandbox; but a profile registers
only pure modules in the sandbox, and an embedder that wants it there has
`with_std_modules(StdModules::BRIDGE)`.

**The names** `stores` and `store` are taken, by Astra's observables and by **Module store**, and
tidings owns Store, Change feed and Resync. *Ledger* and *Atom* were proposed and set aside for
`bridge.table` and `bridge.cell`. In Rust the types are named through the module, `bridge::Table`
and `bridge::Cell`, since `Cell` would be mistaken for `std::cell::Cell`.

## Consequences

`table.insert(t, 1, value)` on a Bridge table of n items, outside a batch, tells n + 1 changes, one
for each slot it shifts; `table.remove` and `table.sort` do the like. Appending is one.
`bridge.table.insert` and `bridge.table.remove` exist for this.

Operating on a Bridge table is longer to write than a method call would be. A script that does it
often keeps `local bt = bridge.table`.

A subscriber that writes sees changes out of order: the one it made is told to it before the one it
was told of reaches the subscribers after it.

When an object nobody closed is closed by collection is up to the garbage collector. A producer that
is finished closes it.

A Rust subscriber that keeps an `mlua::Value` of its own Bridge table keeps it alive, and so undoes
the weak hold. This is documented rather than prevented.

A script that checks `type(t) == "table"` refuses a Bridge table, and code that walks tables with
`next` or raw access sees none of its contents.
