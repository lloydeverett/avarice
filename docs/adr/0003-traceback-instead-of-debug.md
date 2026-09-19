---
status: accepted
---

# No `debug` library, in either profile — only `debug.traceback`

Neither profile opens Lua's `debug` library. Both get a `debug` table holding a single function,
`traceback`, implemented in Rust over `Lua::traceback`.

A reader would otherwise expect **trusted mode** to have `debug`: it is part of the standard
library, and trusted mode is meant to be the unrestricted one.

## Considered options

`mlua` refuses `StdLib::DEBUG` on a safe Lua state, because `debug.setmetatable`,
`debug.setupvalue` and `debug.upvalueid` can violate the invariants mlua's own safety rests on —
so this is a choice about `Lua::unsafe_new`, not about `debug` in isolation.

**Building trusted mode on `Lua::unsafe_new`** was rejected. It would make the two profiles
differ in their *Rust* safety posture rather than only in what Lua can reach, and a single
unsafe constructor in the codebase invites the next reach for it. [ADR
0002](0002-host-registers-modules.md) established that we do not need it at all.

**Nothing at all** was rejected because `xpcall(f, debug.traceback)` is how Lua code has always
obtained a stack trace, and losing it would be felt on the first error in a real program.

**A larger curated subset** — `getinfo`, `sethook`, `getlocal` — was rejected for now as
speculative. `traceback` covers the idiom; the rest can be added if something actually needs it.

## Consequences

`luaL_traceback` is a plain C API call, so `debug.traceback` works with no library open, and
sandboxed code gets good errors without getting introspection over the host's frames.

Code that feature-detects with `if debug.getinfo then` finds it missing and takes its fallback
path, which is the correct outcome. Code that assumes the whole library is present breaks with a
clear `attempt to call a nil value` rather than silently misbehaving.

Errors returned to Rust carry a traceback already: mlua installs `luaL_traceback` as the message
handler for every protected call it makes, so `avrt` prints one without asking for it.
