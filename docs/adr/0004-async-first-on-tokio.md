---
status: accepted
---

# Async-first, on a current-thread tokio runtime the core owns

> **Amended 2026-09-20, 2026-09-21 and 2026-09-24; the amendments at the end win.** `send` is on,
> so `Runtime` is `Send + Sync` and its lazy-module loaders must be too, there is no `LocalSet`,
> Ctrl-C is not "selected against the running evaluation" but reaches it through the
> `CancelHandle`, and a time limit's clock runs while a chunk waits but does not cut the wait
> short. The text below is the original decision, and says otherwise on the rest of those points
> or leaves them unclear.

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
singleton it controls; avarice hands `Runtime` to embedders.

## Consequences

`Runtime` stays `!Send`, and `register_lazy_module`'s non-`Send` loader keeps
compiling. Tasks are green threads on the thread the Lua state lives on, which
is the only honest reading anyway — one Lua state is not reentrant.

An embedder on `async-std` or `smol` cannot drive avarice; the core owns the
executor rather than leaving the choice open. This was chosen deliberately over
exposing a bare future, to keep task spawning and the runtime's lifetime in one
place.

A time limit now arms across awaits, so a chunk blocked on a slow HTTP response
burns its wall-clock budget without executing Lua. This is accepted: the limits
exist for untrusted code, and untrusted code gets no stdlib module with Rust behind it (only the
pure ones, which are Lua; see ADR 0007).

`avarice` waits for every outstanding task before exiting. Its REPL drains tasks to
completion between prompts, so `spawn_interval` holds the terminal until Ctrl+C
— which aborts the evaluation and every task, and says so on stderr.

## Amendment, 2026-09-20: `send` is on

The considered option above, **`mlua`'s `send` feature**, was rejected and is now taken. The
reversal follows from [ADR 0006's amendment](0006-stdlib-derived-from-astra.md): the stdlib
modules are Astra's own files, and Astra's `utils.rs` puts tasks on `tokio::spawn`, which
requires the Lua state and every function and userdata handed to it to be `Send`. Keeping `send`
off would mean editing that file to use `spawn_local`, which is exactly the kind of change the
amendment exists to avoid.

What the rejection predicted has happened, and is accepted:

- `Modules`' `Rc`/`RefCell` became `Arc`/`Mutex`, and so did `Limits`' `Rc`/`Cell`.
- `Runtime::register_lazy_module`'s loader must now be `Send + Sync`. The "non-`Send` loader keeps
  compiling" consequence no longer holds.
- `Runtime` is no longer `!Send`. The statement in *Consequences* that it stays so is superseded.
  It is `Send` and `Sync` because mlua's `send` feature guards the Lua state with a lock; that
  buys no parallelism inside Lua, and one Lua state is still not reentrant.

Unchanged: the core owns a current-thread tokio runtime, and tasks are green threads on the thread
that drives it. `send` changes what must be `Send`, not which thread runs Lua.

## Amendment, 2026-09-21: no `LocalSet`

The opening paragraph put a `LocalSet` beside the tokio runtime, so that tasks could
be `!Send`. With `send` on that reason is gone: Astra's tasks are `tokio::spawn`ed, Lua futures are
`Send`, and a plain current-thread runtime drives them all. Keeping a `LocalSet` would make
`Runtime` `!Send` again — `LocalSet` is not `Send` — and undo the amendment above for no benefit.

So `Runtime` owns a `tokio::runtime::Runtime` built with `new_current_thread` and `enable_all`,
and `Runtime::block_on` is that runtime's own `block_on`. Everything else here stands: it panics
with tokio's message when called from inside another runtime, tasks are green threads on the
thread that drives it, and they run only while something is driving it. `Runtime` declares the
executor before the Lua state, so that dropping one drops outstanding tasks first.

## Amendment, 2026-09-21: what becomes of tasks, and how Ctrl-C reaches a chunk

Two things the decision above left to `avarice` need the core's help, and neither was in reach of a
CLI written over `block_on` alone.

**Tasks cannot be listed, so they are counted and aborted through the executor.** Astra's tasks
are bare `tokio::spawn`s whose `JoinHandle`s live inside Lua userdata, so the runtime holds no
handle it could wait on or abort, and changing Astra to hand it one is the kind of edit this
project avoids. What tokio does offer is `num_alive_tasks`, and dropping a runtime drops its
tasks. So `Runtime` gains `outstanding_tasks` (that count), `wait_for_tasks` (poll it, every 5ms,
until it is zero) and `abort_tasks` (replace the executor with a fresh one and drop the old, which
ends every task on it and reports how many there were). The executor therefore sits behind an
`RwLock`: `block_on` holds it for reading, and `abort_tasks` takes it for writing without waiting,
panicking with a clear message instead of deadlocking if it cannot, as `block_on` does inside
another runtime. The Lua state is untouched by an abort. The old executor is shut down with
`shutdown_background`, so that it does not wait for blocking work its tasks had started. The
count is of every task on the executor, including any a stdlib module spawns for itself.

The count alone cannot say why a task is gone. A task that was running Lua when a limit fired or a
cancel arrived is stopped by the same hook as any chunk, and Astra prints and swallows a task's
error, so it leaves the executor looking like one that finished. `wait_for_tasks` therefore asks
the limits once more when the count reaches zero, and fails if the latch or the cancel flag is
set. A test that sent the signal at the wrong moment passed without this, so the tests that pin it
send it while the task is the one spinning.

**A signal cannot be selected against a busy chunk.** The decision above says Ctrl-C is
"selected against the running evaluation". That cannot work alone: the executor is a single
thread, and a Lua loop that never awaits never gives it a turn, so a signal it is meant to read
sits unread. And the opposite case fails differently: a chunk that is *awaiting* runs no Lua for
the limit hook to interrupt. So `CancelHandle::cancel` now does both. It sets the flag the hook
reads, and it wakes a `tokio::sync::Notify`, which `Runtime::run` — what `exec` and `eval` are
made of — races the chunk against, dropping the chunk if it wins. `avarice` catches Ctrl-C with
`tokio::signal::ctrl_c` on a thread of its own, whose one job is to trip the handle. The
`CancelHandle` doc's old promise, that it "can stop a runtime from another thread", is now true of
a runtime that is waiting as well as one that is running.

Deliberately not done: a second Ctrl-C that kills the process outright, as stock `lua` has. A
script stuck in a blocking call that never returns to Lua, `io.read` on a terminal for one, notices
the first Ctrl-C only when the call returns; Ctrl-\ (SIGQUIT) still ends it.

## Amendment, 2026-09-24: a time limit does not interrupt a wait

"A time limit now arms across awaits" means the clock runs while a chunk waits, not that the wait
is cut short. The limit is enforced by the hook, which runs only when Lua does, so a chunk waiting
on a response that never comes, or on a `process` Child that never exits, waits on; the limit
stops it once the wait ends and Lua runs again. Only a cancel ends the wait itself. This is
accepted for the same reason as above: every module that can wait on something outside the Lua
state is trusted-only, and `process.run` has a `timeout` of its own.

`Runtime::wait_for_tasks` used to be the exception: it read the clock as it polled, so a time
limit ended that wait at the deadline even with no Lua running. It now reads only what the hook
has latched, and so follows the same rule: a task idling on a timer is waited for past the
deadline, and the wait gives up as soon as a task's Lua runs and is stopped. That is still what
ends the wait for an interval that never finishes, at its first tick past the deadline, and the
wait gives up then rather than leave the interval reporting the limit on every tick after.
