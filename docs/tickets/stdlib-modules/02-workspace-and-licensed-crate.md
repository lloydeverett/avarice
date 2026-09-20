# 02: The workspace split and the licensed stdlib crate

**What to build:** the licence boundary. The repository becomes a workspace with
a second crate, `avarice-rt-stdlib`, carrying Apache-2.0 and a `NOTICE` for
ArkForge Labs, so that derived code and its obligations sit in one unit before
any derived code exists.

Nothing is exposed to Lua by this ticket. It is a prefactor: make the change
easy, then make the easy change.

Recorded in [ADR 0006](../../adr/0006-stdlib-derived-from-astra.md).

**Blocked by:** None (can start immediately).

**Status:** done, in its verbatim-sources form (see the amendment to ADR 0006).

- [x] The repository root is a workspace manifest with the existing package still in place and still building.
- [x] `avarice-rt-stdlib` exists as a workspace member, depends on `mlua` and never on `avarice-rt`, and is `publish = false`.
- [x] It carries the full Apache-2.0 text as `LICENSE`, and a `NOTICE` naming ArkForge Labs. Astra ships no `NOTICE`, so nothing is inherited; this one is ours.
- [x] Its `README` explains what the crate is derived from, the per-file header contract, and — superseded — why `datetime` alone carries no Astra header. Under the amendment `datetime` is Astra's like the rest, so there is no such inconsistency to explain; the README describes `UPSTREAM.md` instead.
- [x] The repository root remains unlicensed. Only this crate has an obligation.
- [x] `avarice-rt` depends on it unconditionally, with no feature flag. This is deliberate and its cost is recorded in ADR 0006: every embedder links the stdlib's dependencies whether or not any Lua calls them.
- [x] `cargo build` and `cargo test` are green at the workspace root.
