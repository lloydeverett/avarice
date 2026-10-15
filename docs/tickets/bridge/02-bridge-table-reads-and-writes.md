# 02: `bridge` module with a Bridge table Lua can read and write

**What to build:** `require("bridge")` in a trusted runtime gives `bridge.table(init?)`, which makes
a **Bridge table**: a userdata that a script reads and writes as a Lua table (`t.k = v`, `t[k]`,
`#t`, `pairs`, `ipairs`, the `table` library), over a backing table. `init` is copied shallowly. No
subscribing yet. The module exists end to end: Cargo feature `stdlib-bridge`, a `StdModule` variant
in every build, registered by trusted mode and not by sandbox mode. See ADR 0022.

**Blocked by:** None (can start immediately)

**Status:** ready-for-agent

- [ ] `bridge.table` is callable, and makes a Bridge table; `bridge.table(init)` copies `init`, so
      later writes to `init` do not show through
- [ ] Reads, writes, removal by nil, `#`, `pairs`, `ipairs` and `table.insert`/`remove`/`concat`/
      `sort`/`unpack` behave as on a plain table
- [ ] Keys named `close`, `subscribe`, `batch`, `insert`, `remove` and `snapshot` read and write
      like any other key
- [ ] Any Lua value can be stored; a stored table is stored by reference
- [ ] `__tostring` gives `bridge.table: 0x...`
- [ ] Trusted mode registers it when compiled in; sandbox mode does not; `with_std_modules` can add
      it to a sandbox
- [ ] The compiled-in, features and sandbox suites and the feature-matrix script know `bridge`, and
      the matrix passes with `bridge` alone
- [ ] A new `bridge` test suite, shaped like the `tidings` one
