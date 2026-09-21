---
status: accepted
---

# `print` is replaced, and the replacement is written in Lua

Both profiles replace Lua's `print` with one that renders tables structurally
rather than as `table: 0x...`. That replacement is written in Lua, over a small
Rust function that writes bytes to the runtime's **write sink**.

A reader would otherwise expect the opposite. Every other capability here is a
**host function** in Rust, Rust is faster, and the code this is adapted from —
Astra's `pprint` — is Rust. Writing one host function in Lua instead needs a
reason.

## Considered options

**Astra's implementation, as-is**, was rejected: it is not safe to expose to
sandboxed code. It formats non-string values with `{:#?}`, which reaches
`mlua`'s `Table::fmt_pretty`, and that has two problems measured against mlua
0.12.1.

It recurses in Rust once per level of table nesting. Cycles are handled — a
`visited` set is checked — but depth is not, because each level is a distinct
pointer. In a release build, `local t = {} for i = 1, 30000 do t = {t} end`
costs about 2.4 MB of Lua memory, far under the sandbox's 128 MB cap, and
printing it overflows the Rust stack and aborts the process. `pcall` cannot
catch it and the debug hook cannot interrupt it, because no Lua is executing.

It also indents with `" ".repeat(ident + 2)` at every level, making output
quadratic in depth: 250 levels produced 126 KB, 1 000 produced 2.0 MB, 10 000
produced 200 MB. No limit bounds this — the memory cap covers Lua's heap, not
what a Rust formatter writes.

**A bounded pretty-printer in Rust** — explicit depth, element and byte budgets
— was the first recommendation and remains a reasonable design. It was rejected
because the budgets would be ours to get right, by inspection, and a later
contributor extending the printer has to rediscover why each budget is there.

**Leaving `print` alone in sandbox mode** and pretty-printing only in trusted
mode was rejected: the two profiles should differ in what Lua can *reach*, not
in how it renders a table.

## Consequences

Safety comes from limits that already exist rather than from budgets written for
this one function. Recursion consumes the Lua stack, which raises a catchable
`stack overflow` instead of aborting the process. String building is charged to
the Lua allocator, so the sandbox's memory cap bounds output. An infinite
`__tostring` is interrupted by the debug hook.

`print` is slower than a Rust implementation. This is accepted: it is a
debugging aid, and the amount of Lua that runs per printed value is small next
to a write syscall.

Metamethod reentrancy is not a new hazard — stock `print` already invokes
`__tostring` — so this changes nothing there.

Being a Lua global, `print` can be replaced by trusted code, exactly as stock
`print` can.

Output goes to the runtime's **write sink** rather than to Rust's `stdout`
directly. Astra writes with `print!`, which would interleave badly with `io`'s C
stdio buffering; trusted mode opens `io`, so we cannot inherit that.

The mlua behaviour above is arguably a bug there — a `Debug` impl should not
abort on safe input — but we are not relying on it being fixed, and this
decision does not depend on the outcome either way.

## Amendment, 2026-09-21: what the implementation settled

The default sink flushes C stdio before it writes. This is the interleaving hazard the section
above names, and the write sink alone does not remove it: Rust's `stdout` and `io.write`'s C
buffer are different buffers, so on a pipe `io.write("a") print("b")` came out as `b` then `a`.
Calling `fflush(NULL)` first puts what `io.write` has written ahead of what `print` is about to.
It is the core's only `unsafe`, and the reason is that there is nothing safe that reaches C's
buffer. A test in `tests/cli.rs` fails without it. The cost is that `fflush(NULL)` flushes every
open C stream, so a file a trusted script has open through `io.open` has its buffer written out
early too; it is done once per `print` call.

Choices the decision left open:

- A table that has a string form of its own — a `__tostring` or `__name` in its metatable — prints
  through it, like stock `print`; every other table prints structurally, with raw access, so no
  `__index`, `__pairs` or `__len` runs. Which is which is decided by asking `tostring` and seeing
  whether the answer is more than the address, because that is the only test that also respects a
  protected metatable (`__metatable`), which `getmetatable` would hide. `__tostring` runs once.
- Keys come out in a fixed order — array part, then numbers, strings, booleans and the rest — so
  that a table prints the same way each time. The exception is a key that is itself a table or
  function, which is ordered by address.
- Only a table containing itself is marked; a table reachable by two paths is printed in full
  both times. That leaves output growth to the memory cap, as this decision intends.
- There is no depth budget. The 30 000-deep case ends at the memory cap (`not enough memory`)
  rather than at a stack overflow, because the indentation is quadratic in depth; either is a
  catchable Lua error.
- The output is built whole before anything is written, so an error leaves the sink untouched.
- A sink that fails makes `print` raise a Lua error. Stock `print` ignores write errors, but it
  also dies of SIGPIPE, which Rust does not; an error is the nearest thing to that which a script
  can see.
