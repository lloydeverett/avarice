# 09: `utils` — tasks, regular expressions, `env.get`

**What to build:** `require("utils")` — and with it, **tasks**: Lua work that
runs concurrently with the chunk that spawned it, so a slow HTTP call does not
stop the rest of a program. Plus regular expressions, for text beyond Lua
patterns, and reading an environment variable, so a script can be configured
from outside.

This is the ticket where the async conversion stops being a shape change and
starts being a feature.

**Blocked by:** 01 (async-first execution), 04 (module selection).

**Status:** ready-for-agent

- [ ] Spawn a task; spawn on a timeout; spawn on an interval; sleep without blocking other tasks. A task can be aborted.
- [ ] A task is a green thread on the one thread the Lua state lives on. Tasks interleave but never run in parallel, so a task never observes a half-finished mutation by another — asserted, not assumed.
- [ ] A task outlives the chunk that spawned it, and `block_on` returns only once outstanding tasks are done.
- [ ] An error inside a task does not take the runtime down.
- [x] Regular expression matching and replacement. In Astra this lives in the `validation` module. That module is now taken whole (see [13](13-validation.md)), so it is `require("validation").regex` and not part of `utils`, and there is nothing left of this bullet to do here.
- [ ] `env.get` reads an environment variable.
- [ ] **`env.set` is not exposed.** It wraps `std::env::set_var`, which Rust 2024 made `unsafe` because it is unsound in a process with threads — Astra's own comment says as much. The crate README records the omission and why.
- [ ] Astra's `clean_require`, `dotenv_load` and `close_all_databases` are not carried over: they reach components we are not taking.
- [ ] The existing limits tests are extended: a time limit and a cancel both reach a spawned task, not only the chunk that spawned it.
- [ ] Carries the attribution header settled in 05, with its own `Changes from the original:` list.
