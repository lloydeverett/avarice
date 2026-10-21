---
status: accepted
---

# A task can be awaited by any number of callers

Any number of callers can `await` the same task at once, and each returns once the task has ended.
A task can be aborted while it is awaited, and aborting it wakes whoever waits on it.

This changes how Astra's code works, which [ADR 0006](0006-stdlib-derived-from-astra.md) otherwise
forbids, so, like [ADR 0015](0015-lua-strings-carry-bytes.md) and
[ADR 0017](0017-the-stdlib-raises-rather-than-leaving-input-out.md), it is an exception recorded as
its own ADR.

## What Astra did

`utils.spawn_task`, `spawn_timeout` and `spawn_interval` return a `TaskHandler`. Its `await` took
the task's `JoinHandle` out of the userdata and held the userdata mutably for as long as it waited.
So while one caller waited, a second `await` raised `error mutably borrowing userdata`, and so did
`abort`. Once the first `await` had returned, the handle was gone, and a later `await` returned at
once, which was right only because the task had ended.

That rules out a whole shape of script: work queued so that each item awaits the one before it,
while whatever queued an item also awaits it to know when it is done. The second waiter raises,
and a `pcall` around it, which such a queue needs so one failure does not stop the rest, turns
"wait for the item before" into "do not wait at all". It was found that way, by an embedder whose
queued work ran at the same time as the work it was queued behind.

## The decision

- **Any number of callers can `await` a task at once.** Each returns once the task has ended:
  run to its end, been aborted, by `abort` or by `Runtime::abort_tasks`, or died. Awaiting a task
  that has already ended returns at once.
- **`abort` works while a task is awaited**, and its waiters return.
- **`await` returns nothing, as before.** It does not say how the task ended.
- **`tostring` reads `TaskHandler(running)`, `TaskHandler(finished)` or `TaskHandler(aborted)`**,
  where `aborted` covers a task that ended without running to its end, including one that died.
  `TaskHandler(awaiting)` and `TaskHandler(awaited or aborted)` are gone: waiting no longer holds
  the userdata, and no longer takes the handle.

The task sends `true` on a `tokio::sync::watch` channel once it has run to its end, and the
`TaskHandler` keeps the receiver. `await` borrows the userdata only to clone that receiver, then
waits for `true` or for the channel to close, which it does when the task is dropped without
sending. `abort` still uses the `JoinHandle`, which stays in the `TaskHandler` for it.
`components/utils.rs` and `lua/utils.lua` record the change in their headers, as ADR 0006
requires.

## Considered options

- **Leaving `await` alone and having scripts poll**, waiting on a short timer of their own until a
  flag says the task is done, was rejected. It works around the defect in every script that hits
  it, and wakes the script every interval for as long as it waits.
- **`tokio::sync::Notify`** was rejected for `watch`. A `Notify` wakes only those already waiting,
  so a caller that starts to wait after the task ended would wait forever. A `watch` channel keeps
  its value after the sender is gone, and closes when the sender is dropped, so a task that is
  aborted or panics wakes its waiters with no code of its own.
- **Having `await` say how the task ended**, as ADR 0017 would prefer, is left for a decision of its
  own: it changes what `await` returns, which is more than this change needs.

## Consequences

A script can now build queues and chains out of tasks. A script that matched on
`TaskHandler(awaiting)` or `TaskHandler(awaited or aborted)`, which only `tostring` gave, reads
`TaskHandler(running)`, `TaskHandler(finished)` or `TaskHandler(aborted)` instead.
