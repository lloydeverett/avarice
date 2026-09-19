---
status: accepted
---

# The stdlib is derived from Astra, and lives in its own Apache-2.0 crate

The **stdlib modules** are adapted from [Astra](https://github.com/ArkForgeLabs/Astra)
by ArkForge Labs, which is Apache-2.0. They live in `avarice-rt-stdlib`, a
separate crate in this repository, so that the derived code and its licence
obligations sit in one unit.

A reader would otherwise wonder why part of this repository carries a licence
and a `NOTICE` when the root carries neither, and why the stdlib is a crate of
its own when it is only ever used through `avarice-rt`.

## Considered options

**A module inside `avarice-rt`** was rejected. Apache-2.0 §4 obligations attach
to the derived files, and a crate boundary is the clearest line to draw them
around. It also keeps the derived dependencies — reqwest, jiff, sha2, sha3,
base64, regex, glob, uuid — nameable as a set.

**Depending on Astra as a library** is not possible: Astra is a binary crate
built around its own `#[tokio::main]`, an axum server and a `tokio::spawn`-based
task model that needs `mlua`'s `send`. None of that survives contact with
[ADR 0004](0004-async-first-on-tokio.md).

**Making the dependency optional, behind a feature** was recommended and
rejected by the author. The consequence is recorded below.

## Consequences

The stdlib crate depends on `mlua` and never on `avarice-rt`, so the dependency
runs one way only. `avarice-rt` depends on it unconditionally — no feature flag
— which means every embedder links reqwest and a TLS stack whether or not any
Lua calls `http`, taking the core's dependency tree from four crates to roughly
a hundred and fifty. This was chosen for simplicity over dependency hygiene, and
reversing it later is a breaking change to every embedder's `Cargo.toml`.

Astra ships no `NOTICE` file, so no `NOTICE` obligations are inherited; the one
in the stdlib crate is ours, naming ArkForge Labs. Only §4(a)–(d) apply, which
the crate's `LICENSE` and per-file headers discharge.

Every derived file carries a header naming the Astra file it came from and a
`Changes from the original:` list that is never empty — a file copied verbatim
still says so. A reader learns what we changed without diffing against a
repository they may not have.

`datetime` is the exception: it is written from scratch against `jiff`, because
Astra's is written directly against `chrono`'s types and porting it would have
produced a rewrite wearing a copy's attribution. It carries no Astra header, and
the crate README says why that file differs from its neighbours.

Adapting rather than vendoring means upstream fixes do not flow to us. This is
accepted: the parts taken are small and stable, and the parts that would have
churned — the HTTP server, templates, database, validation — are not taken at
all.
