---
status: accepted
---

# PUC-Rio Lua 5.4, not Luau

avarice embeds PUC-Rio Lua 5.4 via `mlua`, even though the author's other
project, which embeds Luau and was then also named avarice (this one was
avarice-rt), was the starting reference for this one. Lua 5.4 was originally
chosen because trusted code needed to reach existing Lua C libraries —
`luaposix` was the motivating example — and Luau could not: no `lauxlib.h`, no
`package` library, no dynamic loading, so C modules were impossible there
rather than merely awkward.

avarice no longer supports native C modules at all — see [ADR
0002](0002-host-registers-modules.md) — so that requirement no longer applies.
The choice of Lua 5.4 over Luau still stands, but on the reasons below rather
than that one, and it isn't treated as closed: if a good enough reason to
reconsider Luau turns up, it's worth reopening.

## Considered options

**Luau** was the initial recommendation, on the strength of `lua.sandbox(true)`,
a built-in memory limit, an interrupt hook, and consistency with that other project. Three
findings reversed it. Consistency stopped mattering once avarice was scoped
as an independent sibling with no dependency in either direction. Luau's
safety-by-default turned out to be overstated for our purposes: mlua installs a
filesystem-backed `require` and `loadstring` into every Luau state regardless of
which `StdLib` flags are passed, so that state needs deliberate stripping too.
And Luau is semantically Lua 5.1, not 5.4 — no integers, no bitwise operators,
no `goto`, 32-bit `lua_Integer` — which is a poor fit for system-facing scripting.

One argument against Lua 5.4 was investigated and found false: that 5.4's debug
hooks are per-coroutine, letting a script escape an instruction-count limit by
spawning one. Lua's `lua_newthread` copies hook state to the child. The escape
is real but is an artifact of mlua's `Lua::set_hook`, and is avoided by using
`Lua::set_global_hook` instead.

**Supporting both dialects behind cargo features** was rejected: the dialects
differ in ways that reach the public API — module loading, sandbox primitives,
interrupts versus debug hooks — so it would mean two runtimes under one name.

## Consequences

Sandboxing is ours to build rather than inherited: withhold `io`/`os`/`package`/
`debug`, cap memory at the allocator, force `ChunkMode::Text` to refuse binary
chunks, and use `set_global_hook` for deadlines. `StdLib::ALL_SAFE` must never
be used, as it includes `package`.
