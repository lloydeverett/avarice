---
status: accepted
---

# `tidings` is a stdlib module, not a contributed one

Lua reaches **tidings Stores** through a **stdlib module**, `require("tidings")`, compiled in by
one feature, `stdlib-tidings`, which brings in all three of tidings' backends: the filesystem,
SQLite and memory. avarice depends on tidings as a git dependency pinned to a commit, since tidings
is not on crates.io. Like `dirs` and `datetime`, the module is original to avarice and outside
[ADR 0006](0006-stdlib-derived-from-astra.md)'s scope. It has Rust behind it, so it is not
**pure**, and sandbox mode does not register it.

Every timestamp tidings gives, a File's last-modified time or a Commit's timestamp, reaches Lua as a
`datetime` Timestamp, in every build. `stdlib-tidings` turns on `_datetime_types`, a support feature
in the manner of [ADR 0007](0007-stdlib-modules-are-compile-time-optional.md)'s `_astra_serde`,
which compiles `datetime`'s Rust types without registering `datetime`. A script whose runtime does
not register `datetime` can use the Timestamps it is given, but cannot make new ones.

A Store on the filesystem or SQLite runs tidings' tasks on the runtime's executor, watching or
polling its Location, for as long as it is open. So an open Store counts as a **task**, as a
running **Child** does: `avarice` waits for it to be closed before exiting, and the REPL waits at a
line that leaves one open. If `abort_tasks` ends those tasks, the Store would go on reading and
committing but never again report a Change, and the Lua state, with the Store in it, survives an
abort. So each such Store also runs a sentinel task beside tidings', and if that ends without the
Store being closed, the Store raises from then on, naming `abort_tasks`, and its feed gives one
Resync and ends.

A reader would otherwise wonder why, with [ADR 0020](0020-contributed-modules-register-through-the-builder.md)
giving any crate a way to contribute a module, a binding to a separate crate lives in avarice, and
why the module is not called `store`. They would also wonder why opening a Store keeps `avarice`
from exiting.

## Considered options

**A contributed module**, defined in tidings or in a crate of its own, was rejected. A contributed
module is never part of a **profile**, so every embedder that wanted it would have to add it by
hand, while the point is for a trusted script to have somewhere durable to keep files, as it has
`fs` and `dirs`. tidings and avarice have the same author, and avarice is not published, so the
git dependency costs nothing that a crate boundary would save.

**One feature per backend**, so that an embedder could leave out SQLite, which tidings compiles
from C, was rejected for now. ADR 0007 gives each module one feature, and nobody has yet asked to
save that build.

**Times as RFC 3339 strings** were rejected as a type that every caller must parse before comparing
two times. **A Timestamp only when `stdlib-datetime` is on** was rejected because a value's type
would change with the build. **Refusing to compile `stdlib-tidings` without `stdlib-datetime`**
was rejected because it breaks the matrix's build of each module alone, and a support feature
gives the same result without the refusal.

**Running tidings on a tokio runtime of its own**, one per avarice runtime, on a thread of its own,
was rejected. A Store would then never count as a task, so `avarice` would exit with one open and
the REPL could hold one, and `abort_tasks` could not reach its tasks. But a Store left open is a
mistake worth seeing, as a Child left running is, and the guard above makes an aborted Store fail
where it is used rather than fall silent. It would also cost a thread, and a handoff between
runtimes on every call.

**Calling the module `store` or `stores`** was rejected: `stores` is already Astra's observables,
and a **Module store** is where a program's source comes from. The module takes the crate's name.
