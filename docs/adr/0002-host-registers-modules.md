---
status: accepted
---

# Lua never loads modules; the host registers them

`package` is never opened, in either profile. `require` is our own function,
resolving names against host-registered modules first and then a **module
store**. Lua code therefore cannot cause a `.so` to be loaded, cannot reach
`package.path`, and cannot reach the filesystem through the module system. Which
modules a runtime has — including native ones like `luaposix` — is decided in
Rust at construction, per profile.

A reader may otherwise wonder why we reimplement `require` rather than
installing a custom searcher into the stock one, which would have worked.

## Consequences

`mlua`'s safe constructor is sufficient; `Lua::unsafe_new` is never needed,
because that only exists to let *Lua* load C modules. Native modules are reached
by the host calling `luaopen_*` through `Lua::create_c_function`, which carries
no safety-mode check.

Statically linking a native module — compiling it against the vendored Lua
headers rather than `dlopen`ing a distro `.so` — avoids needing `-rdynamic` to
export Lua symbols, and is the only way native modules can work on Windows. It
requires a direct `mlua-sys` dependency, since `DEP_LUA_INCLUDE` reaches direct
dependents only.

`Lua::preload_module` must not be used: without `package` open it looks up a
nil `_PRELOAD` and silently does nothing. `Lua::register_module` is correct — it
creates `_LOADED` if absent, and our `require` shares that table as its cache.
