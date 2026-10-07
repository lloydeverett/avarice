-- The program, which the app runs from its module store.
local greeting = require("greeting")     -- contributed by the avarice-greeting crate
local config = require("config")         -- registered by the app from Rust
local shout = require("text").shout      -- lua/text.lua, from the same store

print(greeting.hello(config.user))
print(greeting.hello_all({ "Ada", "Grace" }))
print(shout(greeting.hello(config.user)))
