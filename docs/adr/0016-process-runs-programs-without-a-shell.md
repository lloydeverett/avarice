---
status: accepted
---

# `process` runs programs without a shell, and a Child counts as a task

`process` is a new **stdlib module** for running other programs: starting a **Child** from a
**Command**, talking to it through its standard streams, and collecting its **Output**. It is
original to avarice, like `dirs`
([ADR 0014](0014-dirs-resolves-standard-directories-via-etcetera.md)), and replaces what Lua itself
offers: `os.execute` and `io.popen`. Both hand a string to the host's shell, `os.execute` gives back
only the exit status, and `io.popen` gives only one stream and learns the status only when closed.

A reader would otherwise wonder why a runtime that already has `os.execute` and `io.popen` ships a
third way to run a program, why `process.run("ls -la")` is an error, why `avarice` will not exit
while a program it started is still running, and why killing a Child leaves its own children alive.

## The decision

- **No shell, ever.** A Command is a Lua table whose positional entries are the program and its
  arguments, passed one by one. Nothing is split, quoted, expanded or globbed. A bare string is an
  error whose message shows the table form: splitting `"ls -la"` correctly needs a shell's rules,
  and treating it as a program name would fail with a confusing "not found".
- **Two tiers on one Command.** `process.run` starts a Child, waits for it and returns its Output;
  `process.spawn` returns the Child, whose standard streams Lua reads and writes as it runs. `run` is
  `spawn` followed by collecting the Output, so the two cannot disagree.
- **Every call yields its task, and none takes a callback.** Waiting, reading and writing look like
  blocking calls from Lua, as `http`'s `execute` and `fs`'s reads do, and concurrency comes from
  `utils.spawn_task`. `http`'s `execute_streaming`, whose callback runs detached and whose errors
  reach only a log, is deliberately not the model.
- **Captured output is a Buffer.** An Output's stdout and stderr are **Buffers**, held outside the
  Lua state until asked for, and reach Lua as exact bytes ([ADR 0015](0015-lua-strings-carry-bytes.md)).
  Output larger than memory is read piece by piece from a spawned Child instead.
- **A Child that exits unsuccessfully is not an error.** `run` returns its Output, with `ok` false,
  so stdout, stderr and the exit code are always available together. `check = true` raises instead.
  What is raised is always a table, with a `kind` of `"start"`, `"exit"` or `"timeout"` and a
  `__tostring` that reads as a message, so a script can tell failures apart and still print one.
- **A timeout raises, carrying what was captured.** `run`'s `timeout` kills the Child and raises a
  `"timeout"` error holding the partial Output, whether or not `check` is set, as Python's
  `subprocess.run` does.
- **Only the Child is killed, never its descendants.** Python, Go and Node all do the same by
  default. After a Child is killed, by a timeout or a cancelled call, the partial Output is what was
  read before the kill; nothing waits for its pipes to close, since a grandchild that inherited them
  may hold them open indefinitely. That is Python's own fix for the same hang.
- **A Child counts as a task.** A watching task on the **executor** owns every Child, so everything
  the runtime already does with tasks applies unchanged: `outstanding_tasks` counts Children,
  `wait_for_tasks` waits for them, `abort_tasks` kills them, and dropping the `Runtime` kills them.
  `avarice` therefore waits for running Children before exiting, and Ctrl-C kills them, exactly as
  it waits for and aborts tasks. Losing Lua's handle to a Child does not end it, since the watching
  task still holds it; it closes Lua's ends of its pipes, so a Child writing into them gets a broken
  pipe and does not hang `avarice` forever.
- **A program is found the same way on every platform.** A program with a path separator is
  resolved against the Command's `cwd`, as POSIX does, and not against the host's, as Windows does.
  A bare name is searched for on the `PATH` the Child will get, including one the Command's `env`
  overrides, and on Windows with each extension in `PATHEXT`, so `{"npm", "test"}` finds `npm.cmd`.
  The lookup is [`which`](https://docs.rs/which)'s, a new dependency behind a new `stdlib-process`
  feature, beside tokio's `process` feature. Because the lookup happens before the start, "no such
  program" and "no such directory" are told apart, which the operating system alone reports alike.
- **`"inherit"` means the host's own streams**, not `print`'s **write sink**. Routing an inherited
  stream through the sink would mean piping and copying it, and the Child would no longer see a
  terminal: no colour, no prompts, no editors.
- **`print` names the program, never its arguments.** A Child prints as its program, pid and state,
  and an error as its message. Arguments are where tokens get passed, for the reason `http` never
  prints a request's headers.
- **Not pure, so trusted-mode-only**, per [ADR 0007](0007-stdlib-modules-are-compile-time-optional.md).

## Considered options

**Wrapping a mature Rust crate** was looked at and rejected. `duct` and `subprocess` block a thread
per child and cannot be cancelled by dropping a future, which is how every limit and Ctrl-C reaches
Lua here. `async-process` duplicates `tokio::process` for another executor. `processkit` and
`tokio-process-tools` are closest in scope but were a few months old and pre-stable when this was
decided.

**Killing the whole tree**, through `process-wrap`'s Job Objects and process groups, was rejected. On
Unix, a Child in a process group of its own is not the terminal's foreground group, so it no longer
receives the terminal's Ctrl-C, and stops with `SIGTTIN` if it reads the terminal, which breaks
`vim` with inherited streams. No mainstream standard library does it by default.

**Killing a Child when its handle is garbage-collected** was considered first and rejected for
matching how tasks already behave. Collection is unpredictable, and `avarice`'s rule for tasks, wait
for them and let Ctrl-C end them, is one rule a script author already knows.

**A builder**, as `http.request` has, was rejected. Everything about a Command is known before it
runs, and a table is one way to write it where a builder and a table would be two.

## Consequences

A script that starts a long-running program and returns keeps `avarice` running until that program
exits or Ctrl-C is pressed, and at the REPL the prompt waits for it, as it does for
`utils.spawn_interval`. The count of aborted tasks `avarice` reports on Ctrl-C includes Children.

A program that outlives the script, detached from it, is not possible. It can be added later, with
killing a whole tree, as options on a Command.

An embedder that redirected `print` still gets an inherited Child's output on the host's own
streams.
