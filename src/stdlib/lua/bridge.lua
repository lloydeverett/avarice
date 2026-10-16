-- Original to avarice: not derived from Astra. See ADR 0022.
--
-- Values a script writes and reads that the host can watch. A Bridge table is written and read as
-- a Lua table is: `t.k = v`, `t[k]`, `#t`, `pairs`, `ipairs` and the `table` library all work on
-- it, and every key is the script's. `type`, `next`, `rawget` and `rawset` see a userdata. A Bridge
-- cell holds one value, replaced whole, and has methods. `serde` encodes a Bridge table as what it
-- holds and a Bridge cell as its value, nested ones included, and `print` shows them as
-- `bridge.table { ... }` and `bridge.cell(...)`.

---@meta

local bridge = {}

---A Bridge table. Index it, and write to it, as a table.
---@class bridge.Table
---@field [any] any

---Makes a Bridge table, empty or holding a shallow copy of `init`.
---@overload fun(init: table?): bridge.Table
bridge.table = {}

---One change to a Bridge table: the key written, the value it had and the value it has now. Nil
---stands for a key that was not there, or is no longer.
---@class bridge.Change
---@field key any
---@field old any
---@field new any

---What `bridge.table.subscribe` and a Bridge cell's `subscribe` give back.
---@class bridge.Subscription
local Subscription = {}

---Stops the subscriber being called, at once. A second call does nothing.
function Subscription:unsubscribe() end

---Calls `fn` with the changes of every write to `t` from now on, inside the write, once it is in
---place, and `on_close`, if it is given, once when `t` is closed. A subscriber that raises does not
---fail the write or the close: its error goes to standard error, and the other subscribers are
---still told. Subscribing to a closed table raises.
---
---`on_close` is called only when `t` is closed by `bridge.table.close` or by the host. A table
---collected unclosed takes `on_close` with it, uncalled.
---@param t bridge.Table
---@param fn fun(changes: bridge.Change[])
---@param on_close fun()?
---@return bridge.Subscription
function bridge.table.subscribe(t, fn, on_close) end

---Closes `t`, calling every subscriber's `on_close`, Lua's and the host's, once, in the order they
---subscribed. A closed table can be read and walked, but a write to it raises, and so do
---`subscribe`, `batch`, `insert` and `remove`. Closing it again does nothing.
---
---Inside a batch of `t`, the batch's writes are told first, then the close.
---@param t bridge.Table
function bridge.table.close(t) end

---Calls `fn`, and tells every write it makes to `t`, however it makes it, as one list when it
---returns, in the order they were made; a key written twice is in the list twice. A batch inside
---a batch of the same table joins the outermost. If `fn` raises, its writes stay, are told, and
---the error is raised again. It gives back nothing.
---
---`fn` cannot yield: a suspended batch would hold back every other write to `t` until it resumed,
---so a yield inside it raises `bridge.table.batch: fn cannot yield`.
---@param t bridge.Table
---@param fn fun()
function bridge.table.batch(t, fn) end

---As `table.insert`: puts `value` at `pos`, or at the end, moving the items from `pos` on up one.
---Its writes are one batch, so subscribers are told them as one list.
---@param t bridge.Table
---@param pos integer
---@param value any
---@overload fun(t: bridge.Table, value: any)
function bridge.table.insert(t, pos, value) end

---As `table.remove`: takes out the item at `pos`, or the last, moving the items after it down one,
---and gives it back. Its writes are one batch, so subscribers are told them as one list.
---
---Its writes are the ones `table.remove` makes, so on an empty table, or at `#t + 1`, it tells
---the write of nil over nil that `table.remove` makes there, at key 0 or `#t + 1`.
---@param t bridge.Table
---@param pos integer?
---@return any
function bridge.table.remove(t, pos) end

---A plain table holding what `t` holds, copied shallowly, for code that wants a real table. Writing
---to it tells no one and leaves `t` as it was. A closed table can still be snapshot.
---@param t bridge.Table
---@return table
function bridge.table.snapshot(t) end

---A Bridge cell: one value, replaced whole.
---@class bridge.Cell
local Cell = {}

---Makes a Bridge cell holding `value`, or nil.
---@param value any
---@return bridge.Cell
function bridge.cell(value) end

---The value the cell holds. It still works once the cell is closed.
---@return any
function Cell:get() end

---Replaces the value the cell holds with `value`, and tells every subscriber, even when `value` is
---the value it held, so that a table changed in place can be set again to say so. On a closed
---cell it raises.
---@param value any
function Cell:set(value) end

---Sets the value to `fn(old)`, the first value `fn` gives, as `set` does. If `fn` raises, nothing
---is set and the error is raised again. `fn` cannot yield, and on a closed cell it is not called
---and this raises.
---@param fn fun(old: any): any
function Cell:update(fn) end

---Calls `fn(old, new)` on every set of the cell from now on, inside the set, once the value is in
---place, and `on_close`, if it is given, once when the cell is closed. A subscriber that raises
---does not fail the set or the close: its error goes to standard error, and the other subscribers
---are still told. Subscribing to a closed cell raises.
---
---`on_close` is called only when the cell is closed by `close` or by the host. A cell collected
---unclosed takes `on_close` with it, uncalled.
---@param fn fun(old: any, new: any)
---@param on_close fun()?
---@return bridge.Subscription
function Cell:subscribe(fn, on_close) end

---Closes the cell, calling every subscriber's `on_close`, Lua's and the host's, once, in the order
---they subscribed. A closed cell can still be read with `get`, but `set`, `update` and `subscribe`
---raise. Closing it again does nothing.
function Cell:close() end

-- Everything above is for LuaLS. What runs is this: the Rust half hands the module over as a value
-- (see `components/bridge/mod.rs`), and this file returns it.
return ...
