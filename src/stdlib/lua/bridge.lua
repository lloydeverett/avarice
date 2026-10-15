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

-- Everything above is for LuaLS. What runs is this: the Rust half hands the module over as a value
-- (see `components/bridge.rs`), and this file returns it.
return ...
