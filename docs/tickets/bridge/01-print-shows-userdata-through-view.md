# 01: `print` shows userdata through `__view`

**What to build:** `print` shows a userdata whose metatable has a `__view` function and a `__name`
as that name followed by what `__view` returns: `name { ... }` when the view is a table, `name(...)`
otherwise. Core only; no `bridge` needed. See ADR 0023 and the spec, `docs/specs/bridge.md`.

**Blocked by:** None (can start immediately)

**Status:** ready-for-agent

- [ ] A userdata made in a test with `__view` and `__name` prints through its view, table and
      non-table forms
- [ ] A table with a `__view` field prints exactly as before
- [ ] A `__view` that raises falls back to today's output, and `print` does not raise
- [ ] A view that leads back to its own userdata prints as a cycle
- [ ] Tests in the `print` suite, through its buffer write sink
