---
status: accepted
---

# `stores` is removed

Astra's `stores` module, its `observable` and its `pubsub`, is no longer part of avarice, and nor
is its feature, `stdlib-stores`. Nothing replaces it in the stdlib. A script that wants a topic
of callbacks within Lua writes one; one that wants the host to watch a value uses `bridge`
([ADR 0022](0022-bridge-lets-the-host-watch-lua-values.md)).

A reader would otherwise wonder why one of Astra's modules was taken and later dropped, why
sandbox mode registers no stdlib module, and why `StdModules` has no flag at `1 << 6`.

## Why

`bridge` does not do what `pubsub` does: `pubsub` sends events by topic name from Lua to Lua, and a
Bridge cell holds state that is passed around, not looked up. But `bridge` took away the one reason
the stdlib might have had for messaging, which is reaching the host, and what was left did not earn
its place:

- **It is a few dozen lines of Lua**, which a script that wants it can write as quickly.
- **Its topics belong to the whole program.** `require` gives every caller the same module, so
  every part of a program shares one table of topic names, the collisions a registry of names
  invites and that ADR 0022 refused for `bridge`.
- **It was wrong in ways that would have needed changing Astra's file.** An observable's `publish`
  never sets its `value`. `pubsub.unsubscribe(topic)` with no callback removes whichever subscriber
  came last. A subscriber that raises stops `publish`, so those after it are not told. `bridge`
  was designed against each of these.
- **Nothing used it** but its own tests and README.
- **Its name was in the way.** The glossary had to say twice what `stores` was not, and ADR 0022
  had to name `bridge` around it.

## Considered options

**Keeping it as Astra has it**, so that Astra's scripts run unchanged, was rejected. Running
Astra's scripts is not a goal: [ADR 0015](0015-lua-strings-carry-bytes.md) already departs from
Astra where following it would be wrong.

**Removing `pubsub` and keeping `observable`** was rejected. An observable is a Bridge cell the
host cannot see, whose `value` does not follow `publish`.

**Fixing it** was rejected as building a third observer mechanism beside `bridge` and tidings'
Change feeds, for a need nobody has named.

## Consequences

No stdlib module is **pure**. Sandbox mode still registers the pure ones
([ADR 0007](0007-stdlib-modules-are-compile-time-optional.md)), so it registers none, and has only
the core module `ansi`. The rule, the `pure` field and `StdModules::PURE`, which is now empty, are
kept, so that a pure module added later is in the sandbox without another decision.

`StdModule::Stores`, `StdModules::STORES` and `stdlib-stores` are gone, which breaks an embedder
that names them. The bit `STORES` had, `1 << 6`, is left unused, so that no other module's bit
changes.

[ADR 0006](0006-stdlib-derived-from-astra.md) no longer takes `stores`, and
`scripts/check-features.sh` builds one fewer module alone.
