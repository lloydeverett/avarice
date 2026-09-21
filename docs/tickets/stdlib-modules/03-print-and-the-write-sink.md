# 03: `print` and the write sink

**What to build:** `print` shows a table's contents instead of its address, in
both **profiles**, and an **embedder** can redirect where it goes.

At a REPL, inspecting a table is most of what you do, and it is the one thing
stock `print` will not tell you. The replacement is written in Lua over a single
Rust **host function** that writes bytes to the runtime's **write sink**.

Recorded in [ADR 0005](../../adr/0005-print-implemented-in-lua.md), which exists
because the obvious implementation is not safe: the Rust pretty-printer this is
adapted from recurses without a depth bound and aborts the process on input well
inside the sandbox's memory cap.

**Blocked by:** None (can start immediately).

**Status:** done. What the implementation settled is in the amendment to ADR 0005.

- [x] `print` is replaced in **sandbox mode** and **trusted mode** alike, and is identical in both.
- [x] The pretty-printer is written in Lua. Recursion is Lua recursion, so depth is a catchable Lua error, and the string it builds is bounded by the memory cap.
- [x] Scalars format exactly as Lua's own `tostring` does, so `print(1)` and `print("x")` are unchanged.
- [x] A table renders structurally. A cycle is marked rather than followed.
- [x] The sink is settable on the builder and replaceable on a built `Runtime`. The default is the host's standard output.
- [x] The sink is flushed per `print` call, so output interleaves correctly with anything else writing to the same stream.
- [x] **The ADR 0005 case:** a 30 000-deep nested table, built under the sandbox's default memory cap, is printed and produces a catchable Lua error with the process still alive. This test is the entire reason `print` is written in Lua rather than Rust; it must not be quietly deleted when it gets slow.
- [x] Redirecting the sink to a buffer and reading it back is how the other `print` tests assert, in-process, with no subprocess and no stdout capture.
- [x] Added afterwards: a function prints its parameters after its address (see the second amendment to ADR 0005). Lua functions only; a function written in Rust prints as `tostring` does.
