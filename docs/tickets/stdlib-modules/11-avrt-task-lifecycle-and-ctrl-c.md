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

**Status:** ready-for-agent

- [ ] Script mode runs the program, then waits for every outstanding task before exiting. Spawning a task on a script's last line and having it silently do nothing is the worse surprise; a task that should genuinely be abandoned can be aborted.
- [ ] The REPL drains tasks **to completion** between prompts. One rule, explicable in a sentence. The consequence is accepted and documented: spawning on an interval holds the terminal until Ctrl-C, which makes an interval effectively a foreground command.
- [ ] Because nothing needs to run while input is awaited, the REPL's blocking line read stays as it is. No second OS thread, no experimental external-printer feature.
- [ ] Ctrl-C aborts the current evaluation **and** all outstanding tasks, and prints how many tasks it aborted to stderr. Under drain-to-completion the two are not distinguishable from the user's seat anyway: Ctrl-C means stop what you are doing.
- [ ] Ctrl-C at a prompt keeps its existing meaning — abandon the entry being typed, including a half-finished multi-line one.
- [ ] Ctrl-D at a prompt exits cleanly. Under drain-to-completion there are never outstanding tasks at a prompt, so there is nothing for it to hang on.
- [ ] Cancelling one evaluation does not poison the session. The existing cancel handle already resets per top-level execution, so this needs verifying rather than building.
- [ ] This is `avrt`'s own interrupt handling. Astra's Lua-visible shutdown hook is explicitly **not** being reintroduced.
- [ ] `avrt` now configures a cancel handle unconditionally, so the limit hook is always installed and every run pays a little throughput for it. This is the cost of Ctrl-C working; the comment in the CLI explaining that it deliberately did *not* do this is removed along with the behaviour.
- [ ] `--sandbox` gets no stdlib modules, which follows from the profile and needs no new flag.
- [ ] The README's "one gap worth knowing" paragraph is deleted, because the gap is closed.
- [ ] The end-to-end CLI tests are extended: `avrt` waits for outstanding tasks before exiting, and a stdlib module works from a script.

**Known gap, deliberately accepted:** Ctrl-C during a running script is not
covered by an automated test. Driving it means sending a signal to a child
process and racing its handler, which fails on a loaded CI machine for reasons
unrelated to the code. It is verified by hand, and this paragraph exists so the
gap is a decision rather than an oversight.
