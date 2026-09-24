# Spec: the `process` stdlib module, and exact bytes across the stdlib

Status: ready for implementation. Decisions are recorded in
[ADR 0015](adr/0015-lua-strings-carry-bytes.md) and
[ADR 0016](adr/0016-process-runs-programs-without-a-shell.md); terms (**Command**, **Child**,
**Output**, **Buffer**) are defined in [CONTEXT.md](../CONTEXT.md).

The work is two changes, landed in this order:

1. **Exact bytes** (ADR 0015): the stdlib stops turning bytes into tables of numbers or replacing
   invalid UTF-8. Independent of `process`, and reviewable on its own.
2. **`process`** (ADR 0016): the new module, which relies on the Buffer from change 1.

## Problem Statement

A Lua script run by `avrt`, or by an embedder in trusted mode, has only Lua's own ways to run
another program: `os.execute` and `io.popen`. Both go through the host's shell, so every argument
has to be quoted for that shell, differently on Windows and on POSIX, and a filename with a space
or a quote in it is a bug waiting to happen. Neither gives the whole picture: `os.execute` gives
the exit status but no output, `io.popen` gives one stream but not the other, and learns the status
only when closed. There is no way to feed a program input and read its output at once, no timeout,
and nothing that works the same on Windows as on Linux.

Separately, and in the way of doing this well, the stdlib mangles bytes. A Buffer's `bytes()` is a
table of numbers, 16 bytes of memory per byte of data and impossible to turn back into a string
past about a million entries. Its `text()` replaces invalid UTF-8. `fs.write_file`, an HTTP request
body, and `crypto`'s base64 and hashing corrupt or refuse binary data. A program's output is often
binary, or not UTF-8 (Windows console programs commonly write in the OEM code page), so capturing
it through today's Buffer would corrupt it.

## Solution

A `process` stdlib module that runs a program directly, with no shell, from a Command written as a
Lua table: `process.run{"git", "log", "-n", "5", cwd = repo}`. `run` waits for the Child and returns
its Output — whether it succeeded, its exit code, and its stdout and stderr as Buffers — without
raising when the program merely fails. `spawn` returns the running Child instead, whose stdin a
script writes to and whose stdout and stderr it reads line by line or chunk by chunk as the program
runs. Everything yields the calling task, as the rest of the stdlib does, so several programs run
concurrently under `utils.spawn_task`. It behaves the same on Windows, Linux and macOS, including
finding `npm` as `npm.cmd`.

Alongside, every stdlib function that hands bytes to Lua or takes them from Lua does so as a Lua
string holding exactly those bytes.

## User Stories

### Running a program to completion

1. As a script author, I want to run a program by naming it and listing its arguments, so that I
   never have to quote anything for a shell.
2. As a script author, I want an argument containing spaces, quotes, `$`, `*` or `;` to reach the
   program exactly as I wrote it, so that filenames from the outside world cannot break or subvert
   my command.
3. As a script author, I want the exit code, stdout and stderr of a finished program together from
   one call, so that I never have to choose which of them I get.
4. As a script author, I want a program that exits unsuccessfully to return normally with `ok`
   false, so that I can inspect its stderr and decide what to do.
5. As a script author, I want to opt in with `check = true` to an error on unsuccessful exit, so
   that a script that should stop on failure does not have to check every call.
6. As a script author, I want the error raised by `check` to carry the Output, so that a `pcall`
   still gives me the stdout and stderr of the failed program.
7. As a script author, I want an uncaught `check` error to print as a readable message naming the
   program, its exit code and the last line of its stderr, so that a failing script says why.
8. As a script author, I want to give a program input as a string or Buffer, so that I can pipe
   data through a filter program without a temporary file.
9. As a script author, I want a program's stdin to be empty by default under `run`, so that a
   program that reads stdin sees end-of-file instead of hanging on my terminal.
10. As a script author, I want to set a timeout on `run`, so that a hung program cannot hang my
    script.
11. As a script author, I want a timeout to raise an error carrying whatever the program wrote
    before it was killed, so that I can see how far it got.
12. As a script author, I want to merge stderr into stdout, in the order the program wrote them, so
    that I can capture a combined log the way `2>&1` does.
13. As a script author, I want to discard a stream with `"null"`, so that noisy output I do not need
    is neither shown nor held in memory.
14. As a script author, I want to let a program use my terminal directly with `stdio = "inherit"`,
    so that I can run interactive programs like an editor or `ssh` from a script.
15. As a script author, I want to set a program's working directory, so that I can run tools that
    act on the directory they are started in.
16. As a script author, I want to add, override and remove environment variables for a program,
    with `false` meaning remove, so that I can adjust its environment without rebuilding it.
17. As a script author, I want to start a program from an empty environment with `clear_env`, so
    that I can run it with nothing inherited.
18. As a script author, I want a program named with a relative path to be found relative to the
    working directory I gave it, on every platform, so that `{"./build.sh", cwd = "proj"}` means
    what `cd proj && ./build.sh` means.
19. As a script author, I want a bare program name to be looked up on the `PATH` the program will
    actually get, including one I override, so that what runs is what that environment names.
20. As a script author on Windows, I want `{"npm", "test"}` to find `npm.cmd`, so that the same
    script works on Windows and elsewhere.
21. As a script author, I want a program that cannot be started to raise an error table saying why
    (`not_found`, `permission_denied`, `bad_cwd`, or `other`), so that I can fall back when a tool
    is not installed.
22. As a script author, I want to tell the three kinds of failure apart by a `kind` field
    (`start`, `exit`, `timeout`), so that my handling does not depend on parsing messages.
23. As a script author, I want a bare string such as `"ls -la"` to be refused with a message showing
    the table form, so that I learn the right way immediately instead of getting a confusing "not
    found".

### Interacting with a running program

24. As a script author, I want to start a program and keep a handle to it while it runs, so that I
    can talk to it rather than only wait for it.
25. As a script author, I want to read a running program's stdout line by line with a `for` loop,
    so that I can react to each line as it is produced.
26. As a script author, I want to read the next chunk of output as a string, or `nil` at end of
    file, so that I can process output larger than memory.
27. As a script author, I want to read the rest of a stream as a Buffer, so that once I have seen
    what I need I can collect the remainder cheaply.
28. As a script author, I want to write to a running program's stdin and close it, so that I can
    drive programs that take input interactively or in a stream.
29. As a script author, I want to wait for a Child to exit and get its exit status, so that I can
    tell how it ended.
30. As a script author, I want a way to wait that also drains both output streams at once, so that
    a program with a lot of output cannot deadlock against a full pipe.
31. As a script author, I want to ask a Child to stop politely with `terminate`, and force it with
    `kill`, so that a long-running program gets a chance to clean up.
32. As a script author, I want a Child's process id, so that I can report it or hand it to another
    tool.
33. As a script author, I want reading, writing and waiting to yield only my task, so that other
    tasks — including ones driving other Children — keep running meanwhile.
34. As a script author, I want to run several programs concurrently with `utils.spawn_task`, so
    that independent work finishes sooner.
35. As a script author, I want printing a Child to show its program, pid and state, so that I can
    see what is going on while debugging.
36. As a script author, I want printing a Child or an error never to show the program's arguments,
    so that a token passed on a command line does not end up in a pasted log.

### Lifetime

37. As an `avrt` user, I want `avrt` to wait for programs my script started before exiting, so that
    a script's Children are not cut off because the script's last line ran — as with tasks.
38. As an `avrt` user, I want Ctrl-C to kill the programs my script started, as it aborts tasks, so
    that one keypress stops everything.
39. As a script author, I want a program to keep running when I drop my last reference to it, so
    that garbage collection timing never decides whether my program runs.
40. As a script author, I want a program I have abandoned while it writes to a pipe not to hang
    `avrt` forever, so that an unread Child cannot block exit.
41. As a script author, I want a `run` whose task is aborted or whose chunk is cancelled to kill its
    program, so that cancelling the work cancels the work.
42. As a script author, I want killing a program on a timeout to return promptly even if the program
    started children of its own that still hold its output open, so that a timeout means what it
    says.
43. As an embedder, I want dropping a `Runtime` to kill every program its Lua started, so that I
    never find processes I cannot account for.
44. As an embedder, I want `outstanding_tasks`, `wait_for_tasks` and `abort_tasks` to cover Children,
    so that my existing handling of a runtime's background work covers programs too.

### Profiles and builds

45. As an embedder, I want `process` behind its own `stdlib-process` feature, so that I can build
    without it and without its dependencies.
46. As an embedder running untrusted Lua, I want sandbox mode never to register `process`, so that
    sandboxed code cannot run programs.
47. As an embedder, I want to add `process` to a runtime explicitly, as with any stdlib module, so
    that I decide who can run programs.

### Exact bytes

48. As a script author, I want `buffer:bytes()` to return a Lua string of exactly the buffer's
    bytes, so that binary data survives and costs one byte per byte.
49. As a script author, I want `#buffer` to give its length without copying it into Lua, so that I
    can check whether stderr was empty cheaply.
50. As a script author, I want `buffer:json()` to fail on invalid UTF-8 rather than guess, so that
    corrupt input is an error I see, not data I trust.
51. As a script author, I want `fs.read_file_bytes` to return a string, so that reading a binary
    file gives me its bytes, not a table of numbers.
52. As a script author, I want `fs.write_file` with a string to write exactly its bytes, so that I
    can write binary files.
53. As a script author, I want an HTTP request body given as a string to be sent exactly, so that
    I can upload binary data.
54. As a script author, I want `crypto.base64.encode` and `crypto.hash` to accept any bytes, and
    `crypto.base64.decode` to return exactly the decoded bytes, so that binary data round-trips.
55. As a script author with existing code, I want tables of byte values still accepted as input
    where they were before, so that my code keeps working.
56. As a maintainer, I want each changed Astra file's header to list what changed, so that the
    header stays the one record of how the file differs from Astra's.

## Implementation Decisions

### Change 1: exact bytes (ADR 0015)

- The shared buffer types (both the immutable one `http` uses and the mutable one `fs` uses) change:
  `bytes()` returns a Lua string of exactly the contents; `text()` is removed; `json()` parses the
  contents as UTF-8 and fails on invalid UTF-8, keeping its existing null handling (JSON `null`
  becomes `nil`, as `serde`'s JSON does); `__len` gives the byte length. `__tostring` is unchanged:
  it shows the length, never the contents.
- `fs.read_file_bytes` returns a string. `fs.read_file` keeps failing on non-UTF-8.
- `fs.write_file` given a string writes its exact bytes. Byte tables and JSON-able tables are
  accepted as before.
- An HTTP request body given as a string is sent as its exact bytes; the `text/plain` default
  content type is unchanged. Byte tables are accepted as before.
- `crypto.hash`, `crypto.base64.encode` and its URL-safe twin accept any bytes; both decoders
  return exactly the decoded bytes.
- Rust-side, this means taking Lua strings as byte strings rather than as `String`, and returning
  byte slices as Lua strings rather than as vectors or lossily converted text.
- Every changed Astra-derived file records the change under `Changes from the original:`.
- The README's `response:body():text()` example becomes `:bytes()`.

### Change 2: the `process` module (ADR 0016)

**Module and build.**

- A new stdlib module named `process`, original to avarice-rt, with a Rust half and a Lua half on
  the same two-layer shape as `dirs`: Rust registers `astra_internal__`-prefixed primitives, and a
  Lua file builds the public API over them and carries the annotations.
- A new `stdlib-process` feature turns on tokio's `process` feature, the `which` crate, and the
  shared buffer types. It joins the `stdlib` feature set and the stdlib module registry as the
  tenth module, not pure, so trusted mode registers it and sandbox mode does not. The feature-check
  script builds it alone like the others.
- Uses `tokio::process` for spawning and `std::io::pipe` for merging stderr into stdout. No shell is
  involved on any platform.

**Command** — a Lua table:

| Field | Meaning | Default |
|---|---|---|
| `[1]` | the program | required |
| `[2..n]` | arguments, passed one by one | none |
| `cwd` | working directory | the host's |
| `env` | table of variable → string to set, or `false` to remove | none |
| `clear_env` | start from an empty environment before `env` | `false` |
| `stdin` | `"pipe"`, `"inherit"`, `"null"`, or a string or Buffer to feed and then close | `run`: `"null"`; `spawn`: `"pipe"` |
| `stdout` | `"pipe"`, `"inherit"`, `"null"` | `"pipe"` |
| `stderr` | `"pipe"`, `"inherit"`, `"null"`, `"stdout"` (merge) | `"pipe"` |
| `stdio` | sets all three at once; a per-stream field wins over it | — |
| `check` | `run` only: raise on unsuccessful exit | `false` |
| `timeout` | `run` only: milliseconds before the Child is killed | none |

- A non-table Command, a string in particular, is an error whose message shows the table form.
  Validation happens eagerly, before anything starts, as `dirs.app` validates its arguments.
- Arguments are byte strings. On Windows, where command lines are UTF-16, an argument that is not
  valid UTF-8 is an error.
- `"inherit"` means the host process's own standard streams, not the runtime's write sink.

**Program resolution**, done by avarice-rt before starting, identically on every platform:

- A program containing a path separator is resolved relative to the Command's `cwd` (or the host's
  working directory if none is given).
- A bare name is searched for on the `PATH` the Child will receive, after `env` and `clear_env` are
  applied, using `which`; on Windows this honours `PATHEXT`.
- Resolution failing is a `"start"` error with `reason = "not_found"`; a `cwd` that does not exist
  is `reason = "bad_cwd"`, checked separately so the two are told apart.

**`process.run(command)` → Output**

- Output is a plain Lua table: `{ ok = boolean, code = integer|nil, signal = integer|nil, stdout =
  Buffer, stderr = Buffer }`. `signal` is set only on Unix when the Child was killed by a signal,
  and then `code` is `nil`. A stream that was not piped gives an empty Buffer; so does `stderr` when
  merged into `stdout`.
- Reads stdout and stderr concurrently while waiting, so a full pipe cannot deadlock.
- Unsuccessful exit returns normally. With `check = true` it raises an `"exit"` error.
- On `timeout`, the Child is killed and a `"timeout"` error is raised regardless of `check`,
  carrying the Output with what was read before the kill. After the kill, `run` waits for the Child
  to exit but not for its pipes to reach end of file.
- If the future driving `run` is dropped — the task aborted, the chunk cancelled, the time limit
  hit — the Child is killed.

**`process.spawn(command)` → Child** (userdata)

| Member | Behaviour |
|---|---|
| `child.stdin` | a writer, or `nil` if not piped: `:write(string \| Buffer)`, `:close()` |
| `child.stdout`, `child.stderr` | a reader, or `nil` if not piped |
| reader `:read()` | the next chunk as a string, `nil` at end of file |
| reader `:line()` | the next line without its line ending, `nil` at end of file |
| reader `:lines()` | an iterator over `:line()` |
| reader `:rest()` | everything remaining, as a Buffer |
| `:wait()` | yields until exit; returns `{ ok, code, signal }`. Documented: may hang if a piped stream fills and nobody reads it; use `:output()` |
| `:output()` | drains both piped streams concurrently, waits, returns an Output |
| `:kill()` | forceful: SIGKILL on Unix, `TerminateProcess` on Windows |
| `:terminate()` | polite: SIGTERM on Unix; the same as `:kill()` on Windows |
| `:pid()` | the operating system's process id |
| `__tostring` | `Child(<program>, pid <n>, running)` or `Child(<program>, pid <n>, exited <code>)` — never the arguments |

- Every blocking operation yields the calling task; none takes a callback.
- Two tasks using the same reader at once: the second gets an error, not interleaved data.

**Errors** — always a table with a `__tostring`:

| `kind` | Raised by | Fields | Message |
|---|---|---|---|
| `"start"` | `run`, `spawn` | `program`, `reason` (`not_found`, `permission_denied`, `bad_cwd`, `other`), `message` (the operating system's text) | `process: could not start 'foo': program not found` |
| `"exit"` | `run` with `check` | `program`, `output` | `process: git exited with code 128: <last line of stderr>` |
| `"timeout"` | `run` with `timeout` | `program`, `output` (partial) | `process: git timed out after 5000 ms` |

`program` is the name as the Command gave it. No field or message holds the arguments.

**Lifetime**

- Every Child is owned by a watching task spawned on the runtime's executor, which holds the OS
  process with kill-on-drop. The Lua-side handle shares state with it, so waiting and killing work
  through either.
- Consequences, needing no change to the core: `outstanding_tasks` counts Children;
  `wait_for_tasks` waits for them, so `avrt` does not exit while one runs and the REPL waits between
  prompts; `abort_tasks` (Ctrl-C) drops the watching tasks and so kills the Children; dropping the
  `Runtime` does the same.
- Garbage-collecting the Lua handle does not kill the Child. It closes Lua's ends of any pipes,
  so a Child writing to them gets a broken pipe.
- Only the Child itself is ever killed; programs it started are not.

## Testing Decisions

**What a good test is here.** Each test drives the module the way a script author or embedder would
— Lua source through the public `Runtime` API, or the `avrt` binary — and asserts on what comes
back or what is printed. No test reaches into the module's Rust types, its `astra_internal__`
primitives, or how the watching task is built. A test names the behaviour it pins, in the style the
existing tests use (`a_timeout_ends_the_wait_for_tasks`).

**Seams.** Two, both existing:

1. **Lua through the public `Runtime` API**, a trusted runtime running a chunk and returning or
   printing values, as the stdlib and task tests already do. Almost everything is tested here:
   Command validation, streams and their defaults, merging, `env`, `cwd`, program resolution,
   Output shape, all three error kinds, timeouts, the Child's methods, printing, and — through
   `outstanding_tasks`, `wait_for_tasks` and `abort_tasks` — that Children count as tasks and die
   with an abort or with the runtime. Exact bytes (change 1) is tested here too, through `fs`,
   `crypto`, and `http` against the tests' existing one-shot local server.
2. **The `avrt` binary**, as the CLI tests already drive it, only for what only `avrt` does: waiting
   for a Child before exit, and Ctrl-C killing one (Unix-only, as the existing SIGINT tests are).

**The program the tests run.** Tests need a Child that behaves identically on every platform, so
they do not use `sh`, `echo` or `cat`. The Child is the `avrt` binary itself, given a Lua one-liner:
`io.write` for stdout, `io.stderr:write` for stderr, `os.exit(n)` for exit codes, `io.read` to echo
stdin, a busy loop or a long `utils.spawn_timeout` for a program that does not finish. Its path is
the one Cargo provides for the crate's binary, so these tests need the `cli` feature as well as
`stdlib-process`, and are gated on both. The feature-check script's `process`-alone build therefore
compiles `process` alone but skips these tests; the full build runs them.

**Prior art.** The stdlib tests (registration per profile, `__tostring` of each userdata, never
printing secrets, the one-shot HTTP server), the task tests (counting, waiting, aborting, what an
abort drops), and the CLI tests (a script waits for its tasks, Ctrl-C via SIGINT, killing `avrt`
if it overruns).

**Behaviours to pin, at least:**

- A bare string Command is refused, with the table form in the message.
- An argument with spaces, quotes and `$` arrives unchanged.
- stdout and stderr come back separately; merged, they interleave in write order.
- Non-UTF-8 and NUL bytes in output survive to `bytes()` exactly; `#` gives the length.
- An unsuccessful exit returns `ok = false` and the code; `check` raises `"exit"` carrying the Output.
- A missing program raises `"start"` / `not_found`; a missing `cwd`, `bad_cwd`.
- A timeout kills, raises `"timeout"` promptly with partial output, including when the Child left
  a grandchild holding its stdout.
- `env` sets, overrides and removes; `clear_env` empties; a `PATH` override changes resolution.
- A relative program resolves against `cwd`.
- `spawn`: lines read as written; stdin written then closed reaches the Child; `:output()` on a
  Child with more output than a pipe holds does not deadlock; `kill` and `terminate` end it.
- A Child counts in `outstanding_tasks`; `wait_for_tasks` waits for it; `abort_tasks` kills it;
  dropping the runtime kills it; collecting the handle does not.
- Printing a Child or an error shows the program and never an argument.
- Sandbox mode does not register `process`; trusted mode does.
- Change 1: each changed function round-trips arbitrary bytes, and byte tables are still accepted.

**Windows.** There is no Windows CI. The tests are written to pass there (no shell, no Unix-only
programs, the SIGINT tests already gated), and should be run on Windows by hand before this is
called done — in particular `PATHEXT` resolution, which Linux cannot exercise.

## Out of Scope

- Running through a shell, or any shell-like syntax: pipelines, redirection, globbing, string
  commands.
- Pipelines between Children (`a | b`) as a feature; a script can copy between a reader and a
  writer itself.
- Detaching a program so that it outlives the script, runtime or `avrt`.
- Killing a Child's descendants, process groups, Job Objects.
- Sending arbitrary signals; `kill` and `terminate` only.
- A timeout on `spawn` or on individual reads; a script holding a Child can kill it from a task.
- Pseudo-terminals, for driving programs that insist on a terminal while being captured.
- Routing inherited streams through `print`'s write sink.
- Changing `os.execute` or `io.popen`; they stay as Lua ships them.
- Anything in sandbox mode.

## Further Notes

- `fs`'s `file:read(buffer)` into a fresh buffer reads nothing, since a new buffer has capacity but
  no length (checked: it returns 0), and `read_buf` appends with no way to consume what it read.
  That is why `process`'s readers return strings and do not copy `fs`'s read-into-buffer shape.
  Fixing `fs` is not part of this work.
- A Child's captured output lives outside the Lua state, so the runtime's memory cap does not see
  it. That is acceptable because `process` is trusted-only, as ADR 0004 already accepts for other
  modules with Rust behind them.
- The README gains a `process` example, and `stdlib()` and the CONTEXT.md list of stdlib modules
  gain `process`, when the module lands.
