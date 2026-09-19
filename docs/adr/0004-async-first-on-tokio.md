---
status: accepted
---

# Async-first, on a current-thread tokio runtime the core owns

`Runtime` has no synchronous entry points. `exec` and `eval` are gone, replaced
by async ones, and the core crate owns a current-thread tokio runtime with a
`LocalSet` on it. **Stdlib modules** reach the network and the filesystem
through it, and Lua code spawns **tasks** onto it.

Two things a reader would otherwise wonder about. Why a library that embeds a
synchronous interpreter has no synchronous way to run a chunk. And why commit
1d168e6 removed an `async` feature that this brings straight back — the answer
being that it removed a feature exposing nothing, and this adds one that does.

## Considered options

**A blocking stdlib** — `std::fs`, a blocking HTTP client, no tokio — was the
first recommendation and was rejected. It cannot have `spawn_task` at all, and
the three async-shaped modules are the ones worth having.

**Keeping the sync entry points alongside the async ones** was rejected as a
trap. An mlua async function called under a sync `exec` yields with nothing
driving it, so `http.get` inside `exec` fails at runtime rather than at the type
level. One way to run a chunk is worth more than two, one of which half-works.

**`mlua`'s `send` feature with a multi-threaded runtime**, which is how Astra
does this, was rejected. It would require every registered function and every
userdata to be `Send`, turning `Modules`' `Rc`/`RefCell` into `Arc`/`Mutex`
throughout, and it buys no parallelism inside Lua: `send` puts a lock around the
Lua state. Astra can afford it because its Lua state is a process-wide
singleton it controls; avarice-rt hands `Runtime` to embedders.

## Consequences

`Runtime` stays `!Send`, and `register_lazy_module`'s non-`Send` loader keeps
compiling. Tasks are green threads on the thread the Lua state lives on, which
is the only honest reading anyway — one Lua state is not reentrant.

An embedder on `async-std` or `smol` cannot drive avarice-rt; the core owns the
executor rather than leaving the choice open. This was chosen deliberately over
exposing a bare future, to keep task spawning and the runtime's lifetime in one
place.

A time limit now arms across awaits, so a chunk blocked on a slow HTTP response
burns its wall-clock budget without executing Lua. This is accepted: the limits
exist for untrusted code, and untrusted code gets no stdlib modules.

`avrt` waits for every outstanding task before exiting. Its REPL drains tasks to
completion between prompts, so `spawn_interval` holds the terminal until Ctrl+C
— which aborts the evaluation and every task, and says so on stderr.
