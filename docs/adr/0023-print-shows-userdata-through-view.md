---
status: accepted
---

# `print` shows userdata through `__view`

A userdata whose metatable has a `__view` is shown by `print` as what `__view` returns, after the
metatable's `__name`. `print` calls `__view(value)`, takes the one value it returns, and shows it as
it shows any value: a table laid out with its keys, as `bridge.table { a = 1 }`, and anything else
in parentheses, as `bridge.cell(42)`. A `__view` that raises, or a userdata with no `__name`, is
shown as it is today.

`print` trusts `__view` only on userdata. On a table it is an ordinary field, and is ignored.

A reader would otherwise wonder why `print`, which reads a table raw so as never to run a script's
`__index` or `__pairs`, runs a function it finds in a metatable, and why a table cannot do the same.

## Why userdata alone

`print` reads raw because a table's metatable is the script's, and a script's `__index` or
`__pairs` could raise, loop, or change what it is asked to show. A userdata's metatable is not the
script's: only Rust can give userdata a metatable, since neither profile has `debug.setmetatable`,
and Lua 5.4 has no `newproxy`. So a `__view` on userdata is the host's code, and running it is
running the host, as calling any host function is.

A table's `__view` would be the script's, with every reason `print` reads raw against it.

## Why a protocol, and not `bridge`

**Bridge tables** and **Bridge cells**
([ADR 0022](0022-bridge-lets-the-host-watch-lua-values.md)) are what want it first, but `print` is
core and `bridge` is a stdlib module that a build may leave out. `print` asking Rust whether a value
is a Bridge table would tie the core to one module, behind a feature. A field in the metatable ties
it to nothing, and any **host module** whose userdata is a view of data can give one.

## Considered options

**`__pairs` on userdata**, which `print` would walk as it walks a table, was rejected: `__pairs` is
for iterating, which a userdata may give for reasons that have nothing to do with how it looks, and
a value that is not a table, a Bridge cell's, has nothing to iterate.

**`__tostring`** already lets userdata choose its text, and `print` honours it. It gives one line of
text, which `print` cannot lay out or highlight as the table it stands for.

**Showing the view with no marker**, exactly as a plain table, was rejected: a reader could not
tell from the output that writing to it is seen by the host.

## Consequences

A `__view` that is slow, or that loops, holds up `print`. It is the host's code, as a slow host
function is.

A view that contains the userdata it came from, or another whose view contains it, is caught as a
cycle, as a table that contains itself is.
