---
status: accepted
---

# Stdlib modules are compile-time optional

> [ADR 0012](0012-the-stdlib-is-a-directory-not-a-crate.md) makes the stdlib a directory of
> `avarice-rt` and not a crate of its own, so the features below are declared once, on `avarice-rt`,
> and are not forwarded from a stdlib crate. Where the text says otherwise, it wins.

Each **stdlib module** is behind a Cargo feature, so an embedder that wants only `crypto` and
`serde` does not build reqwest, a TLS stack and the rest for a `http` it will never register. A
feature decides whether a module is **compiled in**. It does not decide whether a runtime has it:
that is still the **profile**'s job, and trusted mode registers every module that is compiled in
while sandbox mode registers only the **pure** ones: those written entirely in Lua, with no Rust
behind them. All eight are on by default, so an embedder that changes nothing sees nothing change.

This reverses the rejection recorded in [ADR 0006](0006-stdlib-derived-from-astra.md), which chose
an unconditional dependency for simplicity and named reversing it as a breaking change to every
embedder's `Cargo.toml`. It is not breaking here, because the default keeps everything on; it is
only hard to do later, which is why it is done now.

A reader would otherwise wonder why `StdModule::Http` exists in a build with no `http`, why
`StdModules::ALL` does not always mean eight modules, and why a feature named `stdlib-validation`
turns on code from `utils.rs` without registering `utils`.

## The decision

- **One feature per module**, on the stdlib crate and forwarded by `avarice-rt` under the same
  name: `stdlib-http`, `stdlib-fs`, `stdlib-crypto`, `stdlib-serde`, `stdlib-datetime`,
  `stdlib-utils`, `stdlib-stores`, `stdlib-validation`. `stdlib` turns on all eight and is in
  `default`, beside `cli`. Opting out is `default-features = false` plus the modules wanted. Each
  module's dependencies are `optional`, behind its feature.
- **Turning on one module never registers another.** Where a module's Rust needs another's,
  internal support features compile that code without adding the module: `http` needs the Rust in
  `astra_serde.rs` and the shared buffer types, `fs` needs the buffer types, and `validation`
  needs `AstraRegex` from `utils.rs`. So `stdlib-http` alone gives `require("http")` and nothing
  else. The support features are not part of the API.
- **The types keep their shape.** `StdModule` has every variant and `StdModules` every flag in every
  build, with the bit values fixed, so the enum an embedder matches on does not vary with features.
- **`StdModules::ALL` is the modules compiled in.** So `StdModule::ALL` stops being a fixed
  `[StdModule; 8]`. `Profile::Trusted` and `stdlib()` follow it.
- **Asking for a module that is not compiled in is an error.** `RuntimeBuilder::build` returns one
  naming the feature that is missing. `without_std_modules` on such a module is a no-op, since it
  asks for less. `avarice_rt_stdlib::loader` is public and takes any variant, so for one that is not
  compiled in it returns an error naming the feature when called, rather than panicking.
- **Five public items are added to say what is compiled in:** `StdModule::is_compiled_in`,
  `StdModule::feature` (the name of the feature that compiles it in), `StdModules::not_compiled_in`,
  `StdModules::require_compiled_in` and its error, `NotCompiledIn`. The last is the one message,
  which names the modules and the features, that both `build` and `loader` give. With the changes
  above, the full list of changes to the public API is `StdModule::ALL`, the meaning of
  `StdModules::ALL`, the error from `build`, and these five additions. `StdModules::all()`, the
  method bitflags generates, still returns every flag, compiled in or not.
- **Sandbox registers the pure modules, and no others.** A **pure module** is written entirely in
  Lua, so it reaches nothing outside the Lua state and runs under every limit the runtime puts on
  Lua: the memory cap and the time limit. [ADR 0006](0006-stdlib-derived-from-astra.md) records why
  a module with Rust behind it cannot promise that. Purity is declared once per module, in the
  stdlib crate, beside its name and feature, and `StdModules::PURE` is the pure modules that are
  compiled in, as `StdModules::ALL` is all of them. `Profile::Sandbox` starts from `PURE` where it
  started from `NONE`.

## Considered options

**Features as a security boundary** was rejected. Cargo features are additive and unify across the
dependency graph: any crate that depends on `avarice-rt` with `stdlib-http` turns it on for every
crate that does, so an embedder cannot rely on a feature being off. Confinement stays what
[ADR 0002](0002-host-registers-modules.md) made it, a runtime that does not register the module,
and a module compiled out is only a smaller build.

**Compiling the enum's variants out with their modules** was rejected. It would make the type an
embedder matches on differ from one build to the next, and a match that compiles against one
feature set fails against another, so a library that depends on avarice-rt could not name the
variants at all.

**Dropping a module that is not compiled in, silently,** was rejected on the precedent of ADR 0002:
`build` refuses `StdLib::PACKAGE` and `StdLib::DEBUG` rather than dropping them, so that a runtime
does not quietly disagree with what was asked for. A `Profile` or a builder that names `http` in a
build without it is a mistake worth reporting.

**Sandbox registering no module at all** was the rule until a module with nothing to withhold
existed. It withholds nothing from `stores` and would withhold nothing from `ansi`, and it costs a
sandboxed program the use of code it could have written itself. The rule is now that sandbox
withholds what has Rust behind it, which is where the capability is and where Lua's limits stop
applying.

## Consequences

**Sandbox is not empty.** `stores`, which is Lua all the way down, is registered in a sandbox from
this decision, and so is any pure module added later; `stdlib()` in a sandbox lists them. A test
that asserts a module is unreachable in a sandbox asserts it for the modules that are not pure, and
asserts the reverse for those that are. A pure module says so where it is defined, in the stdlib
crate's README and in the runtime's documentation, so that giving one a Rust half is a visible
decision: it moves the module out of the sandbox.

**The verbatim rule gains one kind of change.** A file cannot be split to isolate a module's half of
it, so a feature gates the *file*, by a `#[cfg]` on its `mod` declaration in `components/mod.rs`.
That file also holds the shared buffer types and helpers, which are gated in place, along with
`#[cfg_attr(..., allow(dead_code))]` where a build with only some features leaves part of a file
unused. [ADR 0006](0006-stdlib-derived-from-astra.md) allows removals, respellings and additions that
alter nothing; attributes that decide what is compiled, and alter nothing when every feature is on,
are a fourth kind, and `components/mod.rs`'s `Changes from the original:` lists each. No other Astra
file changes.

**What stays unconditional.** mlua's `async`, `send`, `serialize` and `macros` are on in every
build. `send` in particular puts a `Send + Sync` bound on the core's loader type, and a bound that
came and went with features would make the core's public API differ between builds. tokio's `net`
follows `stdlib-http`, and the stdlib crate's tokio `fs` follows `stdlib-fs`; the core keeps
`rt`, `sync` and `time`, which it uses itself.

**Tests and the matrix.** A test that needs a module is gated on its feature, and the sandbox
tests still assert that no module that is not pure is reachable. `scripts/check-features.sh` runs `cargo test` over
ten builds: no modules, all of them, and each module alone. Each module alone is what finds a
dependency put behind the wrong feature. There is no CI, so the script is the check.

**Documentation.** The README's build section, the `Profile` docs and `lib.rs` describe compiled in
against registered, and the earlier design's line saying there is no feature flag points here.

## Amendment, 2026-09-21: `ansi` makes nine

[ADR 0008](0008-ansi-is-original-and-pure.md) adds a ninth module, `ansi`, so where the text above
says eight modules, or that `stdlib` turns on eight features, it is nine, and the matrix in
`scripts/check-features.sh` is eleven builds and not ten: no modules, all of them, and each of the
nine alone. Nothing else above changes, and the sandbox rule is the one recorded in the decision.

## Amendment, 2026-09-21: `ansi` leaves, and it is eight again

[ADR 0011](0011-print-highlights-and-ansi-is-a-core-module.md) moves `ansi` into the core, where it
is registered in every runtime and no feature selects it. `stdlib` turns on eight features again,
`stdlib-ansi` is gone, and the matrix in `scripts/check-features.sh` is ten builds: no modules,
all of them, and each of the eight alone. The sandbox rule stands, and `stores` is now the only
pure stdlib module; `ansi` is a pure *core* module, registered by the core whatever the profile.
