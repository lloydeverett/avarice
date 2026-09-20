# 04: Module selection, proven end to end with `stores`

**What to build:** the first **stdlib module** a script can actually reach, and
the mechanism that decides whether it is there.

**Trusted mode** registers stdlib modules; **sandbox mode** registers none; and
an embedder can take trusted mode and subtract one. `stores` is the tracer
bullet: it is the only Astra module with no Rust half at all — roughly ninety
lines of pure Lua — so it proves registration, laziness, gating, the embedded
Lua layer and the attribution header without dragging a single dependency in
behind it.

**Blocked by:** 02 (the workspace and the licensed crate).

**Status:** done, in its verbatim-sources form (see the amendment to ADR 0006).

Not blocked by 01. `stores` is synchronous and pure Lua; if the async conversion
has already landed the tests use `block_on`, and if it has not they do not.

- [x] A `StdModule` enum, one variant per module, each knowing its own `require` name, and a `StdModules` flags type over it with `ALL` and `NONE`. Uses `bitflags`, which mlua already depends on, so it adds nothing to the tree.
- [x] `avarice-rt-stdlib` exposes a loader per module, returning the module's value, built on demand. This is the whole surface between the two crates: the core never learns what a module contains.
- [x] `Profile::std_modules()` sits next to the existing `Profile::std_libs()` — `ALL` for trusted, `NONE` for sandbox.
- [x] `RuntimeBuilder` gains `std_modules`, `with_std_modules` and `without_std_modules`, mirroring the standard-library trio exactly, so an embedder learns the shape once.
- [x] No module is refused the way `StdLib::PACKAGE` and `StdLib::DEBUG` are. There is nothing here that breaks an invariant the runtime rests on, only capability the embedder is entitled to grant.
- [x] Registration goes through the existing lazy-module path, not a new mechanism. This is what makes "a stdlib module carries no privilege your own module lacks" true rather than merely claimed.
- [x] A stdlib module is not built until it is required. Asserted through `has_module` and by observing which module `require` returns when the **module store** holds one of the same name.
- [x] A bare stdlib name shadows a store module of that name. Accepted deliberately; the README's `--path` section says so.
- [x] `stores` is reachable as `require("stores")` in trusted mode: the observer pattern and pubsub. Astra's `stores.lua` has no key/value store, so there is none; the spec's "in-memory key/value" is not something Astra provides.
- [x] Pubsub ships the two-argument `subscribe(topic, callback)` that Astra's code implements, not the three-argument form Astra's own documentation describes. The documented form cannot work — it would bind the observable as the callback and drop the real one. Our docs match our code.
- [x] Trusted registers all seven names; sandbox registers none, and each fails with the runtime's ordinary "module not found" message.
- [x] `without_std_modules` on trusted removes one and leaves the other six. `with_std_modules` on sandbox adds one and leaves the other six absent — a profile is a set of defaults, not a constraint.
- [x] The existing sandbox tests are extended, not replaced: no stdlib module is reachable, and neither is the network or the filesystem through one.
- [x] The README's profile table gains a stdlib row.
