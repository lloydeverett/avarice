---
status: accepted
---

# The stdlib is derived from Astra, and lives in its own Apache-2.0 crate

> Amended 2026-09-20: the sources are Astra's own, not adapted. Amended 2026-09-21: `validation`
> is taken after all, and every Astra file carries a header, with no `NOTICE` or `UPSTREAM.md`.
> Amended 2026-09-21 again: a change may also be an **addition** that leaves everything Astra does
> as it was, and the first is a `__tostring` on the `fs` userdata.
> Amended 2026-09-24: there may be exceptions where a change alters how Astra does things, when
> following Astra would be wrong. Each is its own ADR; the first is
> [ADR 0015](0015-lua-strings-carry-bytes.md), on how bytes cross into and out of Lua.
> [ADR 0012](0012-the-stdlib-is-a-directory-not-a-crate.md) moves the stdlib from its own crate to a
> directory of `avarice-rt`, licenses the whole project Apache-2.0 with one `LICENSE` at the root,
> and folds the crate's `README.md` into the root one; where the text below calls it a crate, says
> the licence is its boundary, or points at its README, it wins.
> Where the text below says otherwise, or mentions `jiff`, `NOTICE`, or files without a header, or
> says validation is not taken, the amendments at the end win.

The **stdlib modules** are adapted from [Astra](https://github.com/ArkForgeLabs/Astra)
by ArkForge LLC, which is Apache-2.0. They live in `avarice-rt-stdlib`, a
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
rejected by the author. The consequence is recorded below. (Reversed by
[ADR 0007](0007-stdlib-modules-are-compile-time-optional.md), which puts each module behind its own
feature.)

## Consequences

The stdlib crate depends on `mlua` and never on `avarice-rt`, so the dependency
runs one way only. `avarice-rt` depends on it unconditionally — no feature flag
(reversed: see [ADR 0007](0007-stdlib-modules-are-compile-time-optional.md)) — which means
every embedder links reqwest and a TLS stack whether or not any Lua calls `http`, taking the core's dependency tree from four crates to roughly
a hundred and fifty. This was chosen for simplicity over dependency hygiene, and
reversing it later is a breaking change to every embedder's `Cargo.toml`.

Astra ships no `NOTICE` file, so no `NOTICE` obligations are inherited; the one
in the stdlib crate is ours, naming ArkForge LLC. Only §4(a)–(d) apply, which
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

## Amendment, 2026-09-20: the sources are Astra's own, not adapted

The stdlib crate now holds Astra's files themselves, byte for byte where they can be and
otherwise changed only by **removal**, each removal noted in a header at the top of the file. The
first attempt adapted the modules — renamed internals, rewrote `datetime` and `http.lua`, cut and
reshaped what remained — and became too hard to review, since every file was a diff against an
upstream nobody could read alongside it. The rule instead is that a change may take functionality
away but may not alter how anything that remains works.

Consequences, where they differ from the decision above:

- **No `astra_internal__` rename.** It changes how a module works, not what it does, so the
  prefix stays. The rename the design first called for, and the `Changes from the original` line that
  carried it, are dropped.
- **`datetime` is Astra's, on `chrono`.** The from-scratch `jiff` rewrite is dropped, and with it
  the crate README's explanation of why one file lacks a header.
- **All of Astra's serde formats are taken** — JSON5, YAML, TOML, INI, CSV, XML as well as JSON —
  along with `serde_yaml`, published as `0.9.34+deprecated`. They cost nothing to keep verbatim;
  cutting them is an ordinary removal to make later.
- **Headers are for files that differ.** An identical file has none, because a header would make
  it not identical. `NOTICE` and `UPSTREAM.md` carry the attribution for those, with a checksum of
  each Astra file. This narrows the earlier rule that every derived file carries a header and a
  never-empty change list.
- **Every dependency is one Astra already has**, at Astra's version requirement. The cost of
  putting the derived dependencies in one crate is now higher: `serde_yaml`, `quick-xml`, `csv`,
  `toml`, `chrono` and `reqwest` join `sha2`, `sha3`, `base64`, `regex`, `uuid` and `glob`.
- **The crate's edition is 2024 and `rust-version` is 1.94**, as Astra's, because its sources use
  let-chains. The workspace's `rust-version` follows.

## Amendment, 2026-09-21: `validation` is taken

The decision above lists validation among the parts not taken, and the first amendment carried
that forward. That left regular expressions with no way to reach Lua: Astra's `utils.rs` holds the
Rust half of `regex`, so it arrived with the tasks, but the Lua wrapper that exposes it is in
`validation.lua`. The alternatives were adding a `regex` function to Astra's `utils.lua`, which is
a change to a file that is otherwise removals only, or taking `validation.lua` whole. Taking it
whole keeps every Astra file as Astra has it, and was chosen.

So there are now **eight** stdlib modules, and `validation` is the eighth: `StdModule::Validation`,
`StdModules::VALIDATION`, `require("validation")`, registered by trusted mode like the others.
`lua/validation.lua` is byte-identical to Astra's (and, under the next amendment, carries the header
every Astra file does). It brings Astra's schema
validators (`types.struct`, `array`, `union`, `range`, `pattern`, `build` and the rest) as well as
`regex`, and the regex is `require("validation").regex`, not `utils.regex` as the design first
planned.

Two things follow from how the file is written, and are handled in `modules::load`, which is ours,
rather than in the file:

- **Its regex primitive is registered by the `utils` Rust half**, so `validation` registers it
  too. It does not depend on `utils` having been required.
- **It defines its functions as globals.** Run as a plain chunk, that puts `number`, `struct`,
  `regex` and a dozen more into every program's globals the moment anything requires the module,
  and breaks it the day a program reuses one of those names. It is run against a table of its own
  that reads through to the real globals, so those definitions stay inside it.

**What taking it costs, which is more than it first looked.** The `regex` crate matches in linear
time, but the Rust code around it neither counts against Lua's memory cap nor can be interrupted
by the time limit, which is a hook that only runs between Lua instructions. `captures` builds its
whole result in Rust before Lua sees any of it, and was measured, in a release build, at about
220 bytes of Rust memory for every byte of input: `regex("(.)"):captures(s)` on a 1 MiB string
peaked at 228 MB, on 4 MiB at 882 MB and on 16 MiB at 3.5 GB, taking 13 seconds, in a sandbox that
would have refused a 128 MiB Lua allocation. A pattern that is expensive to compile can overrun a
time limit likewise. This is Astra's code, taken as it is, and it is true of every stdlib module's
Rust half in some degree. It is why the sandbox profile registers no module that has one (it
registers the pure ones, which are Lua only; see ADR 0007), and why an
embedder who adds `validation` to a sandbox is not getting the limits the sandbox otherwise
promises for anything that reaches it. It is also a reason to look again at whether a module
should be allowed to run Rust that Lua's limits cannot see; that is not settled here.

## Amendment, 2026-09-21: every Astra file carries a header, and there is no `NOTICE` or `UPSTREAM.md`

The first amendment left a file that is byte-for-byte Astra's with no header, on the ground that
a header would make it not identical, and put its attribution in `NOTICE` and `UPSTREAM.md`
instead. That put the attribution in the wrong place: a reader of `crypto.rs` saw nothing to say
whose it was or under what licence. It is reversed.

- **Every file under `src/components/` and `lua/` opens with a header** naming the Astra file, the
  version and commit, the copyright holder and the licence, and a `Changes from the original:`
  list. A file that is otherwise unchanged says `none`, and is byte-for-byte Astra's below its
  header. So the earlier rule that the list is never empty holds again; the rule about changes
  being removals and respellings only is unchanged.
- **`NOTICE` is deleted.** Astra ships none, so none is inherited (§4(d)), and the copyright line
  is in every header. §4(a) is met by `LICENSE`, §4(b) and §4(c) by the headers.
- **`UPSTREAM.md` is deleted.** Its per-file table and SHA-256 checksums were the attribution the
  headers now carry, and are redundant with them: the Astra commit is named in each header, and
  stripping a header leaves Astra's file, so there is nothing to checksum. What it held besides
  attribution, what is not taken and how `validation` is loaded, is in the crate's `README.md`.
- **`src/lib.rs` and `src/modules.rs` are this crate's own** and carry no header. They make the
  same registration calls Astra's `register_components` makes, which the API leaves no other way to
  spell, and share no other expression with Astra.

## Amendment, 2026-09-21: a change may add, if nothing Astra does is altered

The rule was that a change may take functionality away but may not alter how anything that remains
works, which in practice meant removals and respellings only. It is relaxed by one clause: a change
may also **add** something, provided every thing Astra does still does what it did.

The reason is `print`. A userdata's contents live in Rust, and Lua can neither enumerate them nor,
since mlua protects the metatable, list the methods that reach them. `print` therefore cannot
render one the way it renders a table; all it can show is `tostring`, which for an Astra userdata
is its type name and an address. `fs.read_dir` returns a list of them, and printing that listing
showed a column of addresses. Only the type can say what it holds, so the type must say it, and
that is a `__tostring` metamethod, which `print` already honours at the top level and inside a
table.

**The preference, going forward, is that a stdlib userdata has a `__tostring`.** A type that is
added, or that Astra adds in a later version we take, is expected to arrive with one, and a type
without one wants a reason, not the other way round. It is the one addition this crate makes
routinely, so it is the one worth stating outright rather than leaving each file to argue for.

Every userdata Astra hands out now has one, and each says what it is without saying more than it
should: a path for `AstraFile` and `AstraDirEntry`; the kind and length for `AstraMetadata`;
`readonly` or `read-write` for `AstraFilePermissions`; the kind for `AstraEntryType`; method and URL
for `HTTPClientRequest`, and status and URL for `HTTPClientResponse`; the pattern for `AstraRegex`;
the state for `TaskHandler`; the length for `AstraBuffer` and `AstraBufferMut`. (`AstraDateTime`
already had one.) Two things are left out on purpose. **Headers and bodies** are not shown for a
request or a response, because a request's headers are where a token lives and `print` is what a
script reaches for when something is wrong, which is when its output gets pasted somewhere. **A
buffer's contents** are not shown either, because a buffer can be any size and `print` should not
be the way a megabyte reaches a terminal.

Where a userdata's Rust side can be busy, `print` must not be what fails. A `TaskHandler` under an
`await` and an `AstraFile` under a `read` are held mutably while they wait, so theirs is a
metamethod *function* that tries the borrow and says `awaiting` or `in use` when it cannot; a
buffer's `try_lock` does the same. Each file's header, under `Changes from the original:`, lists
its own.

The header remains the only record of what differs from Astra: a change of this kind goes there,
in the same file, or it is not made. An addition is limited to what the rule already protected. It
does not change what a method returns, what a function raises, or what any module exposes under a
name Astra already gave it. Anything that would, is not an addition, and the earlier rule applies.

## Amendment, 2026-09-24: exceptions that alter what Astra does

The rules above, removals and additions only and nothing Astra does altered, are the default and
stay so. There may be exceptions: a place where Astra's behaviour is a defect that no addition can
fix, because the defect lives under a name callers already use. Such an exception is its own ADR,
saying what changes and why following Astra would be wrong, and each file it touches still records
the change under `Changes from the original:` in its header, so the header remains the one record
of how a file differs from Astra's.

The first is [ADR 0015](0015-lua-strings-carry-bytes.md): Astra's buffers, `fs`, `http` and
`crypto` turned bytes into tables of numbers or replaced invalid UTF-8, and the stdlib now passes
bytes through exactly.
