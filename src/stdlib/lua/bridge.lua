-- Original to avarice: not derived from Astra. See ADR 0022.
--
-- Values a script writes and reads that the host can watch. A Bridge table is written and read as
-- a Lua table is: `t.k = v`, `t[k]`, `#t`, `pairs`, `ipairs` and the `table` library all work on
-- it, and every key is the script's. `type`, `next`, `rawget` and `rawset` see a userdata.

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

---What `bridge.table.subscribe` gives back.
---@class bridge.Subscription
local Subscription = {}

---Stops the subscriber being called, at once. A second call does nothing.
function Subscription:unsubscribe() end

---Calls `fn` with the changes of every write to `t` from now on, inside the write, once it is in
---place. A subscriber that raises does not fail the write: its error goes to standard error, and
---the other subscribers are still told.
---@param t bridge.Table
---@param fn fun(changes: bridge.Change[])
---@return bridge.Subscription
function bridge.table.subscribe(t, fn) end

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

-- Everything above is for LuaLS. What runs is this: the Rust half hands the module over as a value
-- (see `components/bridge.rs`), and this file returns it.
return ...
