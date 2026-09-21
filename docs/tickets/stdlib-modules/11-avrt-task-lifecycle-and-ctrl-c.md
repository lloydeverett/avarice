# 11: `avrt` task lifecycle, and Ctrl-C

**What to build:** `avrt` behaves correctly now that a program can leave work
running. A script finishes its outstanding **tasks** before the process exits,
the REPL drains them between prompts, and Ctrl-C during running work aborts it
and returns you to the prompt instead of killing the process.

That last one closes a gap the README currently admits to: "Ctrl-C during a
running script is the terminal's default SIGINT, which kills the process...
Interrupting a script back to the prompt needs a signal handler, which needs a
dependency this crate does not yet have." It does now.

**Blocked by:** 09 (`utils` — tasks).

**Status:** done, ahead of ticket 09, whose tasks it needs only in the form Astra already has
them. What the implementation settled, and three departures from the text below, are in the third
amendment to [ADR 0004](../../adr/0004-async-first-on-tokio.md):

- The runtime keeps no list of tasks, so it counts them from the executor and aborts them by
  replacing it: `Runtime::outstanding_tasks`, `wait_for_tasks`, `abort_tasks`.
- Ctrl-C is not selected against the evaluation but trips the `CancelHandle` from a thread of its
  own, which now also wakes a chunk that is awaiting. `Runtime::run` is what does the racing.
- A run that ends in *any* error gives up its tasks, not only one that Ctrl-C ends, so that the
  prompt is always a place where nothing is running. It says how many, and says nothing if none:
  the ticket's "prints how many tasks it aborted" is read as "when there were some".

- [x] Script mode runs the program, then waits for every outstanding task before exiting. Spawning a task on a script's last line and having it silently do nothing is the worse surprise; a task that should genuinely be abandoned can be aborted.
- [x] The REPL drains tasks **to completion** between prompts. One rule, explicable in a sentence. The consequence is accepted and documented: spawning on an interval holds the terminal until Ctrl-C, which makes an interval effectively a foreground command.
- [x] Because nothing needs to run while input is awaited, the REPL's blocking line read stays as it is. No second OS thread, no experimental external-printer feature.
- [x] Ctrl-C aborts the current evaluation **and** all outstanding tasks, and prints how many tasks it aborted to stderr. Under drain-to-completion the two are not distinguishable from the user's seat anyway: Ctrl-C means stop what you are doing.
- [x] Ctrl-C at a prompt keeps its existing meaning — abandon the entry being typed, including a half-finished multi-line one.
- [x] Ctrl-D at a prompt exits cleanly. Under drain-to-completion there are never outstanding tasks at a prompt, so there is nothing for it to hang on.
- [x] Cancelling one evaluation does not poison the session. The existing cancel handle already resets per top-level execution, so this needs verifying rather than building.
- [x] This is `avrt`'s own interrupt handling. Astra's Lua-visible shutdown hook is explicitly **not** being reintroduced.
- [x] `avrt` now configures a cancel handle unconditionally, so the limit hook is always installed and every run pays a little throughput for it. This is the cost of Ctrl-C working; the comment in the CLI explaining that it deliberately did *not* do this is removed along with the behaviour.
- [x] `--sandbox` gets no stdlib modules, which follows from the profile and needs no new flag.
- [x] The README's "one gap worth knowing" paragraph is deleted, because the gap is closed.
- [x] The end-to-end CLI tests are extended: `avrt` waits for outstanding tasks before exiting, and a stdlib module works from a script.

**Known gap, narrowed:** Ctrl-C *in the REPL* is not covered by an automated test,
because it needs a terminal. It was verified by hand through a pty: a spinning
entry, an interval, an await and a timer draining, Ctrl-C at a half-typed `>>`
prompt, an entry that errors with a task running, and Ctrl-D. Ctrl-C during a
script *is* covered, because the race that made it unreliable is removed by
having the script print a line once the handler stands, and sending the signal
only after reading it.
