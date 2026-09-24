---
status: accepted
---

# The sandbox refuses Lua finalizers

A **profile** decides whether Lua may give a table a finalizer, a `__gc` in the metatable it passes
to `setmetatable`. The sandbox refuses them, and trusted mode allows them. The embedder can change
either with `RuntimeBuilder::allow_lua_finalizers`. Where they are refused, `setmetatable` raises
`setmetatable: finalizers (__gc) are not allowed in this runtime`.

Finalizers of userdata made in Rust are not affected. They are the host's code, and run whatever
the profile.

## Why

Lua 5.4 turns hooks off while it runs a finalizer (`GCTM` in `lgc.c`), and hooks are how the time
limit and a cancel stop Lua. So a finalizer that never returns runs for ever, and so does whatever
set off the collection:

- the script, through `collectgarbage` or any allocation that sets off a step;
- the collection the outermost execution guard runs after a stopped execution, to free what the
  stopped chunk was waiting on;
- the host's own `gc_collect`;
- dropping the runtime, since closing a Lua state runs every pending finalizer.

The last two stall the host itself, not just the script, and the cancel exists to stop exactly
this. The sandbox is for code the embedder does not vouch for, so the only safe place for such a
finalizer is nowhere.

## How

Lua marks a table for finalizing when `setmetatable` gives it a metatable that has a `__gc`,
looked up raw, of any value but `nil`. When the table is collected, Lua calls whatever function
is there by then. So:

- **Any `__gc` is refused**, `true` and `false` included, since a placeholder marks the table and
  a function put in later is called.
- **Checking in `setmetatable` alone is enough.** Adding `__gc` to a metatable once it is set
  marks nothing, and the sandbox has no `debug.setmetatable`, so `setmetatable` is the only way
  Lua can give anything a metatable.
- **The refusal is a wrapper written in Lua**, installed with the base-library restrictions, like
  the one that keeps `load` to text. It keeps the only reference to the real `setmetatable`, which
  the sandbox cannot reach: `debug` has only `traceback`. So it is installed before any Lua that
  could keep a reference of its own, and it holds every global it uses from the start, so that a
  script replacing `rawget` or `type` cannot change what it does.
- **Lua's own errors keep their words and the script's line.** The wrapper calls the real
  function under `pcall` and raises its error again from the caller. Without that, a bad argument
  would be reported at a line of the wrapper. An error the real function did not raise itself,
  running out of memory or a limit's, is raised again as it is. Lua then counts running out of
  memory there as an ordinary error, as it does whenever a script catches it with `pcall` and
  raises it again.

It raises rather than dropping the `__gc`, for the reason
[ADR 0017](0017-the-stdlib-raises-rather-than-leaving-input-out.md) gives: a script whose metatable
was quietly changed has had something done that it did not ask for, and nothing tells it so.

## Considered options

**Letting finalizers obey the limits** would mean patching the vendored Lua to leave hooks on in
`GCTM`, and would still not be enough: Lua turns an error raised in a finalizer into a warning, so
the hook's error would not stop the finalizer's caller either. Patching Lua was rejected.

**Refusing only a `__gc` that is a function** leaves the placeholder above open.

**Removing `__gc` from the metatable, or setting it without marking the table**, were rejected
for the reason in *How*: the script would get a metatable different from the one it gave.

**Turning finalizers off whenever a time limit or a cancel handle is set**, in trusted mode too,
was rejected. It would tie together two settings the embedder chooses separately, and change one
without saying so. The builder method's documentation says instead that a Lua finalizer runs with
the limits off.

**A wrapper written in Rust** would report its errors without the script's position, and would
have to write out Lua's argument errors itself.

## Consequences

Sandboxed code cannot clean up through `__gc`. It has nothing outside Lua to clean up, and
`<close>` variables still work: Lua runs `__close` with hooks on, so the limits cover it.

Every `setmetatable` call in the sandbox goes through a Lua function and a `pcall`. This is a small
cost on a common call, and it is accepted.

Refusing Lua finalizers means little in a runtime that allows binary chunks, which can do anything
the VM can. The sandbox refuses both.

A trusted runtime with a time limit or a cancel handle can still be held up by a finalizer. That is
the embedder's choice to make, and trusted mode is for code they vouch for.
