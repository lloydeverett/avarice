---
status: accepted
---

# The stdlib is a directory of avarice, not a crate of its own, and the whole project is Apache-2.0

The stdlib's code moves from `crates/avarice-stdlib`, a workspace member, to `src/stdlib/`, a
directory of the `avarice` crate. The workspace goes with it: the repository is one package.
(This ADR says *directory*, and not *module*, so that *module* keeps the meaning `CONTEXT.md` gives
it.)

Two things follow from that, and are decided together. The project as a whole is licensed under the
Apache License 2.0, with one `LICENSE` at the root and `license = "Apache-2.0"` in the manifest, so
the licence no longer marks the stdlib out from the rest. And the stdlib's own `README.md` is
gone: what a reader needs from it is in the root `README.md`, under "The stdlib's source".

What [ADR 0006](0006-stdlib-derived-from-astra.md) wanted from the crate boundary was that the
derived code and its licence obligations sit in one place. The obligations are now met for the whole
tree, and what the derived code still has is its own directory, and the attribution in each file's
header.

## Why the crate did not earn its keep

- **It was only ever used through `avarice`.** It is `publish = false` and had no other
  consumer, so nothing needed it to be a unit that stood alone.
- **Every feature was declared twice.** `stdlib-http` and its seven siblings existed on the stdlib
  crate, and again on `avarice`, forwarded by name, and the two lists had to agree. A feature the
  core needed for the same reason (`tokio/net` for `http`) had to be named on the far side of the
  boundary as well as the near one.
- **What the core could do depended on a crate it did not control.** mlua's `send` feature arrived
  through the stdlib crate, by unification, and the core's `Send + Sync` loader bound relied on
  it. That is a coupling that ran through Cargo and could not be seen in either source tree.
- **It made two of everything.** Two editions (the core on 2021, the stdlib on 2024 for
  let-chains), two `rust-version`s, a `[workspace.dependencies]` table for one dependency, and a
  `--workspace` on every test command.
- **It never quite fit what was built on it.** `ansi` was put in the stdlib crate by
  [ADR 0008](0008-ansi-is-original-and-pure.md) and taken out again by
  [ADR 0011](0011-print-highlights-and-ansi-is-a-core-module.md), because `print` needs it in every
  build and a stdlib module is optional. The tests for the module table needed `loader`, which the
  core alone exposes, and so they lived in a different package from the code they were about.

## What is kept: it depends on nothing of ours

The stdlib is still self-contained, and the dependency still runs one way. Nothing in `src/stdlib`
reaches anything else in `avarice`; it names `mlua`, `tokio`, `bitflags` and the dependencies of
its own that are behind its features. What the rest of the crate takes from it is three things,
declared in `src/stdlib/mod.rs`: `StdModule`, `StdModules` and `loader`. The core registers a
module's loader as an ordinary lazy module and never learns what any of them contains, as before.

The compiler used to hold that line, because a crate cannot name its dependant. It no longer can,
and nothing replaces it: the line is a convention, kept by review. A test that read the sources for
what they import was written and then deleted. It asserted on the shape of the source and not on
anything the code does, and it grew rules of its own to keep up with that (comments, nested
paths, `include_str!`) which cost more than the drift it guarded against.

**One name is allowed through**, `crate::components`. Astra's `http` files spell their neighbours
that way, and a header is the only record of how a file differs from Astra's, so those files are
not edited (ADR 0006). `src/lib.rs` has a single `use` behind `stdlib-http` that makes the name
resolve. It is the one place the rest of the crate knows something of what is in the directory.

## The licence

**The whole project is Apache-2.0**, the author's own code included. There is one `LICENSE`, the
canonical text, at the repository root. Astra's files still say in their headers that they are
ArkForge's and Apache-2.0, and still list what was changed, which is what §4(b) and §4(c) ask for;
§4(a) is met by the `LICENSE`. Astra ships no `NOTICE`, so none is inherited (§4(d)).

**Confining Apache-2.0 to `src/stdlib`**, with its own `LICENSE` in the directory and nothing on the
rest, was the alternative. It was the arrangement ADR 0006 made with a crate, and a directory could
have carried it. It was rejected because the project has no reason to be under different terms from
the code it is built around, and because it leaves a manifest with no licence, a repository whose
root says nothing, and a published package that would have had to say something that was only
true of part of it.

The costs are that nobody can now take the core without taking Apache-2.0 terms with it, and that
the earlier ADRs' statements that the rest of the repository carries no licence (ADR 0006, and ADR
0011's "the core carries no licence") are no longer true.

The headers say "See LICENSE in this crate's root". They are not edited, because a header changes
when the file's contents do and not otherwise, and they have no need to be: the crate's root is the
repository root, and the `LICENSE` is there. Some also say "which this workspace is on", which was
true of a workspace member and now reads as this package; the README says so beside the licence.

## Considered options

**Keeping the crate** was the alternative, and the reasons above are why it lost. Nothing about
attribution needs it: an Apache-2.0 obligation attaches to the files, and the `LICENSE` plus per-file
headers discharge it wherever the files sit.

**Editing Astra's `http` files to say `crate::stdlib::components`** was rejected for the reason
above. It would have changed two files' bodies and so two headers' `Changes from the original`
lists, to save one line in `lib.rs`.

**A `README.md` in `src/stdlib`**, carried over from the crate, was rejected. It said how the
headers are kept, what was not taken, how a module is built and why some features start with an underscore, and
all of that is about the stdlib as the project ships it, so it is in the root `README.md` where a
reader of the project looks.

## Consequences

- **The whole package is on edition 2024**, because Astra's sources need it. The core compiled as
  it was. Five sites change their drop order (`Runtime::enter`, `Runtime::run`,
  `print::install` and two in the REPL's `run`), each in the same direction: a tail expression's
  temporaries are now dropped before the block's locals and not after. In `Runtime::run` that means
  the future is dropped while its `Execution` guard is still holding the time limit armed, where
  before it was dropped just outside it. Formatting under the 2024 style re-sorted some imports.
- **The lock file loses `avarice-stdlib`**, and the dependency set is otherwise unchanged: every
  dependency the stdlib had is optional in the root manifest, behind the same feature. No
  dependency was added.
- **Features are declared once**, in the root `Cargo.toml`, with the `_astra_*` helpers beside them,
  and `scripts/check-features.sh` runs plain `cargo test`.
- **The module table's tests moved with it**, into `src/stdlib/`, where they can reach `loader`
  without it being public. None were dropped, and none were added.
- **The manifest names its licence**, so a package built from this repository says what it
  contains.
- **References to the design spec and its tickets were reworded, not amended.** ADRs 0004, 0006 and
  0007 pointed at `docs/specs/` and `docs/tickets/` ("ticket 01", "the spec's rename", "the header
  format in the spec"). Those documents are deleted, so the references were reworded to stand
  without them, and a clause that only pointed at the spec was dropped. Nothing else in those ADRs
  changed.

Where an earlier ADR (0006, 0007, 0008, 0009 and 0011 do) names the `avarice-stdlib` crate or its
path, describes the stdlib as a separate crate, a workspace member, or a set of features forwarded
from one manifest to another, or says the Apache-2.0 boundary is a crate, that the rest of the
repository carries no licence, or that the crate's `README.md` says something, this decision wins.
Everything else in them stands.
