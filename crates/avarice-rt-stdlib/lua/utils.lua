-- Derived from Astra <https://github.com/ArkForgeLabs/Astra>, astra/lua/utils.lua
-- Copyright (c) ArkForge Labs, licensed under the Apache License 2.0.
-- See LICENSE and NOTICE in this crate's root.
--
-- Changes from the original:
--   - Removed `clean_require`, `dotenv_load` and `env.set`, and the `astra_internal__` primitives behind them, which `src/components/utils.rs` no longer provides.
--   - Everything else is unchanged.

---@meta

--[[
    All of the smaller scale components that are not big enough to need their own files, are here
]]

---@return string
local function uuid()
  ---@diagnostic disable-next-line: undefined-global
  return astra_internal__uuid()
end

---Represents an async task
---@class TaskHandler
---@field abort fun(self: TaskHandler) Aborts the running task
---@field await fun(self: TaskHandler) Waits for the task to finish

---Starts a new async task
---@param callback fun() The callback to run the content of the async task
---@return TaskHandler
local function spawn_task(callback)
  ---@diagnostic disable-next-line: undefined-global
  return astra_internal__spawn_task(callback)
end

---Starts a new async task with a delay in milliseconds
---@param callback fun() The callback to run the content of the async task
---@param timeout number The delay in milliseconds
---@return TaskHandler
local function spawn_timeout(callback, timeout)
  ---@diagnostic disable-next-line: undefined-global
  return astra_internal__spawn_timeout(callback, timeout)
end

---Starts a new async task that runs infinitely in a loop but with a delay in milliseconds
---@param callback fun() The callback to run the content of the async task
---@param timeout number The delay in milliseconds
---@return TaskHandler
local function spawn_interval(callback, timeout)
  ---@diagnostic disable-next-line: undefined-global
  return astra_internal__spawn_interval(callback, timeout)
end

---@param key string
local function env_get(key)
  ---@diagnostic disable-next-line: undefined-global
  return astra_internal__getenv(key)
end

return {
  uuid = uuid,
  spawn_task = spawn_task,
  spawn_timeout = spawn_timeout,
  spawn_interval = spawn_interval,
  env = {
    get = env_get,
  },
}
