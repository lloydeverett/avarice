---
status: accepted
---

# Bundled modules are Lua that avarice ships and an embedder adds

avarice ships a third kind of **host module**, a **bundled module**: plain Lua, with no Rust half,
built on what the runtime it is added to registers. No **profile** registers one. An embedder adds
it with `RuntimeBuilder::module`, as it would a **contributed module**, and each is behind a Cargo
feature of its own, `bundled-<name>`, all of them turned on by `bundled`, which is in `default`.
The first is `openai`, a client for OpenAI-compatible inference servers that streams chat
completions over `http`'s server-sent events.

A reader would otherwise wonder why `openai` is not a **stdlib module**, when it ships with avarice
and is on by default, and why `HostModule`, which ADR 0020 describes as the shape of a module from
another crate, has an implementation inside avarice.

## Why not a stdlib module

- **A stdlib module stands alone.** None `require`s another, and an embedder can take any one away
  with `without_std_modules`. `openai` needs `http` and `serde` registered and `os` open, so as a
  stdlib module, `Trusted.without_std_modules(HTTP)` would leave it registered and broken, unless
  the stdlib grew a rule about one module needing another, which it has never needed.
- **Purity would stop meaning what it means.** The table's `pure` flag says a module is written
  only in Lua, and the glossary and ADR 0007 take that to mean it reaches nothing outside the Lua
  state, which is why sandbox mode registers every pure module. `openai` is only Lua and reaches
  the network and the environment, so it would have to be the first Lua-only module that is not
  pure.
- **It is closer to a library than to a host module.** It is the Lua a script would write itself
  given `http`, `serde` and `os`, and it is shipped so that every script does not.

## The decision

- **A bundled module is a `HostModule`.** It is a unit struct under `avarice::bundled`, such as
  `bundled::OpenAi`, whose `load` runs its embedded Lua. It goes through the same checks as a
  contributed module: its name may not be taken.
- **What it needs, it `require`s,** when it is first required, and it raises then if the runtime
  does not register one of those modules or does not open a library it uses, naming what is
  missing. `build` does not check: it cannot know what a module's Lua will ask for, and refusing
  `without_std_modules(HTTP)` because `openai` was added would be stricter than any other module is
  held to.
- **Its feature compiles in the stdlib modules it needs.** `bundled-openai` turns on
  `stdlib-http` and `stdlib-serde`. A build with the module but not what it needs could never
  load it. This does not register them: the profile still decides that.
- **No profile adds one.** The `avarice` command adds the bundled modules when it runs trusted, and
  not under `--sandbox`.
- **It reads the environment through `os.getenv`, when a Client is made.** `openai` takes
  `OPENAI_BASE_URL`, `OPENAI_API_KEY` and `OPENAI_MODEL` for what a script leaves out, the first
  two named as OpenAI's own SDKs name them. A variable set to the empty string counts as unset.

## Considered options

**A stdlib module** was rejected, for the reasons above.

**A store of bundled sources**, so that `require` finds `openai.lua` as it finds a program's own
files, was rejected for now. It says most exactly what the module is, but a runtime holds one
store, so it needs stores that chain, for little difference to a script.

**A Rust half that hands it `http`, `serde` and a `getenv`**, as `datetime` is handed its Rust
half, would let it work whatever the runtime registers. It was rejected as more than a Lua client
needs: what the module depends on is what any script in a trusted runtime has.

## Consequences

The glossary has a **bundled module**, and a **contributed module** is no longer the only kind of
host module that implements `HostModule`. `scripts/check-features.sh` builds each bundled module
alone, which catches a feature that does not turn on what its module needs.
