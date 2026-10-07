---
status: accepted
---

# Contributed modules register through the builder; the stdlib keeps its flags

A crate other than avarice can contribute a **host module**. It implements `HostModule`, which
gives the module's name and builds its value, and the embedder passes one to
`RuntimeBuilder::module`. `build` registers it lazily, through the same loader map the stdlib
modules and `Runtime::register_lazy_module` use, so all three behave the same once `require` is
called.

The stdlib keeps `StdModule` and `StdModules`, unchanged. `StdModule` does not implement
`HostModule`, and the trait has no way to say a module is **pure**.

A reader would otherwise wonder why there are two ways to choose a runtime's modules, and why
`build` refuses a name collision that `register_module` lets through.

## Why the builder, and not `register_lazy_module`

`register_lazy_module` already lets a crate add a module, given a built `Runtime`. A builder
method lets an embedder say everything a runtime has in one place, before it exists, and lets
`build` check the whole selection at once. A crate exporting a value that implements a trait is
also easier to use than one exporting a free function with a name for the embedder to repeat.

## Why the stdlib keeps its flags

The flags do three things a list of trait objects does badly:

- **A profile's defaults.** A profile names what it registers: every module **compiled in**, or
  only the pure ones. That needs a closed set whose members avarice knows. A contributed module
  is never part of a profile, because which ones a runtime has is the embedder's decision.
- **Subtraction.** `without_std_modules(StdModules::PROCESS)` is typed. Removing a trait object
  from a list would be done by a name string.
- **The compiled-in check.** `build` refuses a stdlib module whose feature is off
  ([ADR 0007](0007-stdlib-modules-are-compile-time-optional.md)). A contributed module is in the
  build if the crate that defines it is linked, so there is nothing to check.

Making `StdModule` implement `HostModule` would let `.module(StdModule::Fs)` skip that check and
the profile's defaults, giving two ways to register one module that behave differently. The trait
has no purity flag for the same reason: purity matters only to a profile's defaults, which never
include a contributed module.

## Collisions are refused

`build` refuses a contributed module whose name is invalid, or taken by another contributed
module, by a stdlib module the runtime registers, by the core `ansi` module, or by one of Lua's
standard libraries. The last would never be built, since `require` finds the library in `_LOADED`
first. The others would quietly replace a module some other code expects.

A stdlib module the runtime does not register does not take its name: a sandbox may have a
contributed `fs`. `Runtime::register_module` and `register_lazy_module` still replace silently,
since an embedder calling them on a built runtime is acting on that runtime alone, and may mean
to.
