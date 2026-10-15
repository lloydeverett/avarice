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

-- Everything above is for LuaLS. What runs is this: the Rust half hands the module over as a value
-- (see `components/bridge.rs`), and this file returns it.
return ...
