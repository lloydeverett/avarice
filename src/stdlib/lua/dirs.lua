-- Original to avarice-rt: not derived from Astra. See ADR 0014.

---@meta

local dirs = {}

---@class App
---An app's identity, resolved to a path. Every method is pure: it computes where a directory
---would be, and never creates it. Use `fs.create_dir_all` before writing into one.
---@field config fun(self: App): string
---@field data fun(self: App): string
---@field cache fun(self: App): string
---The state directory. Not every platform's strategy has one: `nil` there.
---@field state fun(self: App): string?
---The runtime directory. `nil` when the platform has no notion of one, e.g. no `$XDG_RUNTIME_DIR`.
---@field runtime fun(self: App): string?
local App = {}
App.__index = App

function App:config()
  ---@diagnostic disable-next-line: undefined-global
  return astra_internal__dirs_config(self.app_name, self.author, self.top_level_domain)
end

function App:data()
  ---@diagnostic disable-next-line: undefined-global
  return astra_internal__dirs_data(self.app_name, self.author, self.top_level_domain)
end

function App:cache()
  ---@diagnostic disable-next-line: undefined-global
  return astra_internal__dirs_cache(self.app_name, self.author, self.top_level_domain)
end

function App:state()
  ---@diagnostic disable-next-line: undefined-global
  return astra_internal__dirs_state(self.app_name, self.author, self.top_level_domain)
end

function App:runtime()
  ---@diagnostic disable-next-line: undefined-global
  return astra_internal__dirs_runtime(self.app_name, self.author, self.top_level_domain)
end

---An application's identity, to resolve standard directories for.
---
---`author` and `top_level_domain` only matter on Windows, where they place the app under
---`%APPDATA%\author\app_name`; the `Xdg` strategy Linux and macOS use ignores them.
---@param app_name string
---@param author string
---@param top_level_domain string
---@return App
function dirs.app(app_name, author, top_level_domain)
  return setmetatable({
    app_name = app_name,
    author = author,
    top_level_domain = top_level_domain,
  }, App)
end

return dirs
