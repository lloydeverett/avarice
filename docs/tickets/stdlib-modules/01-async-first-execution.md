# 01: Async-first execution

**What to build:** running Lua becomes asynchronous. An embedder gets one way to
run a chunk, and it is the one that will still work when a **stdlib module**
awaits an HTTP response. Nothing new is exposed to Lua — a program that ran
before runs identically after — but the shape everything else depends on is in
place.

`Runtime::exec` and `Runtime::eval` return futures. The core owns a
current-thread tokio runtime (originally with a `LocalSet` on it; see the status), and `Runtime::block_on`
drives a future on it to completion for callers who are not already async.

Recorded in [ADR 0004](../../adr/0004-async-first-on-tokio.md).

**Blocked by:** None (can start immediately).

**Status:** done, with one departure: no `LocalSet` (see the second amendment to ADR 0004).

This is the wide refactor of the set: it breaks every existing call site at once
— both doctests, the `avrt` command, the REPL, and three of the four integration
test files. There is no useful expand–contract here, because keeping the
synchronous pair alongside is precisely the trap ADR 0004 rejects. It lands as
one change, and it is the only ticket whose diff touches code it does not own.

- [x] `exec` and `eval` return futures; their signatures are otherwise unchanged.
- [x] `Runtime` owns a `tokio::runtime::Runtime` built with `new_current_thread`. There is no `LocalSet`: with `send` on nothing needs one, and it would make `Runtime` `!Send`.
- [x] `Runtime::block_on` drives a future to completion on that runtime, and tasks spawned during the call run while it does. They are not driven between calls and are not waited for.
- [x] Calling `block_on` from inside another tokio runtime panics — tokio's own panic, not wrapped or converted. The documented answer for an embedder already inside tokio is to build the `Runtime` on its own thread.
- [x] mlua's `async` feature is on. Its `send` feature is on too, from the stdlib crate (see the amendment to ADR 0004), so `Runtime` is `Send`; the original `!Send` requirement is superseded.
- [x] `Runtime::enter` and the `Execution` guard are unchanged. A time limit now arms across awaits, which ADR 0004 accepts.
- [x] Dropping a `Runtime` drops its outstanding tasks. Nothing is joined implicitly.
- [x] The existing suite is moved onto `block_on` and passes unchanged in substance — no test's assertion is weakened to accommodate the new shape.
- [x] No `#[tokio::test]` anywhere: the runtime owns its executor, and the tests get the same deal an embedder does.
- [x] `avrt` behaves exactly as it does today, including `--timeout`.
- [x] The crate-level doctests are updated and pass.
