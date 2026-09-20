# Tickets: the stdlib modules

Breakdown of [docs/specs/0001-stdlib-modules.md](../../specs/0001-stdlib-modules.md).
One file per ticket, numbered in dependency order — blockers before the tickets
they block.

| # | Ticket | Blocked by |
| - | ------ | ---------- |
| [01](01-async-first-execution.md) | Async-first execution | — |
| [02](02-workspace-and-licensed-crate.md) | The workspace split and the licensed stdlib crate | — |
| [03](03-print-and-the-write-sink.md) | `print` and the write sink | — |
| [04](04-module-selection-and-stores.md) | Module selection, proven end to end with `stores` | 02 |
| [05](05-crypto.md) | `crypto` | 04 |
| [06](06-serde.md) | `serde` | 04 |
| [07](07-datetime.md) | `datetime` | 04 |
| [08](08-fs.md) | `fs` | 01, 04 |
| [09](09-utils-tasks-regex-env.md) | `utils` — tasks, regular expressions, `env.get` | 01, 04 |
| [10](10-http.md) | `http` | 01, 04 |
| [11](11-avrt-task-lifecycle-and-ctrl-c.md) | `avrt` task lifecycle, and Ctrl-C | 09 |
| [12](12-readme-and-crate-documentation.md) | README and crate documentation | 05–10 |

Three tickets have no blockers and can start in any order: the async conversion,
the workspace split, and `print`. They touch different things.

05, 06 and 07 are synchronous and independent of each other. 05 comes first by
preference rather than by dependency, because it is where the derived-file
conventions get settled against a real file — the header format and the two-layer
Rust/Lua shape that 06, 08, 09 and 10 then follow.

01 is the riskiest ticket and the only one whose diff touches code it does not
own. It is a wide refactor with no useful expand–contract, because keeping the
synchronous entry points alongside the async ones is exactly the trap
[ADR 0004](../../adr/0004-async-first-on-tokio.md) rejects.

**Amended 2026-09-20.** Tickets 02 and 04 are done in their verbatim-sources form; the rest of
this breakdown is unchanged in order but its checklists still describe adapted modules, and each of
05–10 needs rereading against the amendments to
[ADR 0004](../../adr/0004-async-first-on-tokio.md) and
[ADR 0006](../../adr/0006-stdlib-derived-from-astra.md) before it is picked up. What the verbatim
sources give and do not give is listed in each ticket's own status note where it has one.
