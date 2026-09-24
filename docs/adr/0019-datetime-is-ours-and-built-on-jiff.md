---
status: accepted
---

# `datetime` is ours, and is built on jiff

`datetime` is no longer Astra's. It is written from scratch against
[jiff](https://docs.rs/jiff), and its Lua types are jiff's: `Timestamp`, `Zoned`, `Date`, `Time`,
`DateTime`, `Span`, `SignedDuration`, `TimeZone` and `Weekday`. It has left
[ADR 0006](0006-stdlib-derived-from-astra.md)'s scope, as `ansi`, `dirs` and `process` were never
in it. Its files carry no Astra header, since nothing in them is Astra's. `chrono` is gone.

A reader would otherwise wonder why one module breaks the rule that the stdlib is Astra's files
and nothing more. They would also wonder why the 2026-09-20 amendment to ADR 0006 dropped a jiff
rewrite, only for this ADR to bring one back.

## Why not Astra's

Astra's module is one type, `DateTime`, holding a `chrono::DateTime<FixedOffset>`. That is an
instant and an offset, with no time zone, so it cannot do calendar arithmetic across a DST
change. It mutates in place, so `set_day` on a value changes every other reference to it.
`get_weekday` returns a number, and nothing says whether 0 is Sunday or Monday. There is no type
for a span of time, and `sleep` takes a bare number of milliseconds. None of this can be fixed by
an addition, because the defects sit under names callers already use. Fixing them one exception
ADR at a time would rewrite the module while keeping a copy's attribution, which is what the
exception clause in ADR 0006 is not for.

jiff has a type for each of these concepts, and its API is designed so that the wrong thing is
hard to write. Mirroring it gives Lua a model someone can learn from jiff's own documentation.

## Why the 2026-09-20 reason does not apply

That amendment dropped the first jiff rewrite because the first attempt adapted every module,
and each file became a diff against an upstream nobody could read alongside it. That was a
problem of review against Astra. A file written from scratch has no upstream to be read against,
and is reviewed the way `dirs` and `process` are: on its own terms. What made the adapted modules
hard to review, a copy that no longer matches its original, cannot arise when there is no copy.

## The decision

- **The module keeps its name and feature**: `require("datetime")`, `stdlib-datetime`,
  `StdModule::Datetime`. **It is a clean break.** Nothing of Astra's surface survives, and there
  is no compatibility shim: `datetime.new` is gone.
- **The files stay where they are**, `components/datetime.rs` and `lua/datetime.lua`, beside
  Astra's. Each says at the top that it is original to avarice-rt. ADR 0006's header rule is
  amended to cover only files derived from Astra.
- **jiff's names and concepts, with Lua spelling.** `checked_` and `saturating_` are dropped from
  method names, because every method raises on failure
  ([ADR 0017](0017-the-stdlib-raises-rather-than-leaving-input-out.md)). Builders become an
  optional options table. Values are immutable, and operators are metamethods.
- **Time zones come from jiff's default features.** These read the system's zoneinfo where there
  is one, and use bundled data on Windows, where there is none. Like jiff, an unknown system zone
  falls back to UTC, and `TimeZone.try_system` is there for a program that would rather raise.
- **The Rust half is handed to the Lua file as a value**, not through `astra_internal__`
  globals, which were Astra's convention and are not ours to extend.

## Considered options

**Keep Astra's module, with exceptions for its defects**, was rejected for the reason above: the
exceptions would be the whole module.

**A jiff module alongside Astra's**, under a new name, was rejected. The stdlib would have two
date-time modules, and one of them could not be fixed.

**Keeping `datetime.new` as a shim** over the new types was rejected. Nothing depends on this
runtime yet, and a shim would carry Astra's ambiguities forward under a name that suggests they
were chosen.

## Consequences

- `serde` drops userdata it finds in a table, and it is Astra's, so a datetime placed in a table
  to be encoded disappears from the output. The README says to call `tostring` first. Changing
  that would be an exception to ADR 0006 of its own, and is not made here.
- Every Rust half runs outside Lua's limits, so `datetime` stays out of the sandbox
  ([ADR 0007](0007-stdlib-modules-are-compile-time-optional.md)).
- Upstream Astra fixes to `datetime` no longer apply to us, and we do not look for them.
