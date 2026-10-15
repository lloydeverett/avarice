# 04: Batching Bridge table writes

**What to build:** `bridge.table.batch(t, fn)` tells every write `fn` makes to `t`, by any path, as
one list when it returns, in write order, uncoalesced. Nested batches fold into the outermost. If
`fn` raises, the writes made stay, are told, and the error is raised again.
`bridge.table.insert(t, pos, value)` and `bridge.table.remove(t, pos)` shift the sequence as one
batch.

**Blocked by:** 03

**Status:** ready-for-agent

- [ ] Several writes inside `batch` reach a Lua and a Rust subscriber as one list, in order
- [ ] `table.insert`/`table.sort` inside a batch join it
- [ ] A nested batch delivers nothing until the outermost ends
- [ ] A raising `fn`: writes kept, told, then the error re-raised at the `batch` call
- [ ] `insert` and `remove` in the middle of a sequence are one list;
      `table.insert(t, 1, v)` outside a batch is still one notification per shifted slot
