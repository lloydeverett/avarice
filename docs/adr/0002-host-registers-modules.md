---
status: accepted
---

# Lua never loads modules; the host registers them

`package` is never opened, in either profile. `require` is our own function,
resolving names against host-registered modules first and then a **module
store**. Lua code therefore cannot cause a `.so` to be loaded, cannot reach
`package.path`, and cannot reach the filesystem through the module system. Which
modules a runtime has is decided in Rust at construction, per profile.

A reader may otherwise wonder why we reimplement `require` rather than
installing a custom searcher into the stock one, which would have worked.

## Consequences

`RuntimeBuilder::build` refuses `StdLib::PACKAGE` rather than dropping it,
so an embedder who asks for it — most likely by reaching for
`StdLib::ALL_SAFE`, which contains it — gets an error instead of a runtime
that quietly disagrees with this decision. `StdLib::DEBUG` is refused in the
same place, for the different reason given in ADR 0003.

`mlua`'s safe constructor is sufficient; `Lua::unsafe_new` is never needed,
because that only exists to let *Lua* load C modules, and with `package` never
opened, Lua has no path to doing so.

`Lua::preload_module` must not be used: without `package` open it looks up a
nil `_PRELOAD` and silently does nothing. `Lua::register_module` is correct — it
creates `_LOADED` if absent, and our `require` shares that table as its cache.
