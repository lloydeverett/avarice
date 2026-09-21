# 13: `validation`

**What to build:** `require("validation")`, Astra's schema validators and its regular
expressions, as the eighth **stdlib module**. Added after the breakdown was written, because
regular expressions had no way to reach Lua: the Rust half is in Astra's `utils.rs`, and the Lua
that exposes it is in `validation.lua`. Taking that file whole was chosen over adding a `regex`
function to `utils.lua`, so that no Astra file gains a line. See the second amendment to
[ADR 0006](../../adr/0006-stdlib-derived-from-astra.md).

**Blocked by:** 04 (module selection).

**Status:** done.

- [x] `StdModule::Validation`, `StdModules::VALIDATION` and the name `validation`; trusted mode registers it, sandbox mode does not, and it can be added to a sandbox or taken from trusted mode like any other.
- [x] `lua/validation.lua` is Astra's, unchanged: it opens with an attribution header saying `Changes from the original: none`, and below it is byte-for-byte Astra's.
- [x] Regular expressions work: `is_match`, `captures`, `replace` with and without a limit, and a bad pattern is an error a script can catch.
- [x] They work when `validation` is the only module selected, so the module does not lean on `utils` having been built.
- [x] Astra's validators load and run under this runtime: struct, array, union, literal, range, pattern, optional, boolean, nil and `build` with defaults, including the messages they report.
- [x] Requiring the module adds no globals but the primitive, `astra_internal__regex`, which `utils` leaves too; and a script reusing a name the file defines, `number` or `struct`, neither breaks it nor is broken by it. Astra's file defines them as globals; `modules::load` runs it against a table of its own. This is the one place a stdlib module is loaded differently from Astra's way, and it was not part of the choice offered to the user, so it is worth their confirming.
- [x] The docs that state how many modules there are say eight, and the sandbox test derives its list from `StdModule::ALL` so that it cannot fall behind again.
- [x] What taking the module costs is recorded rather than glossed: the Rust behind `regex` is outside the sandbox's memory cap and time limit (ADR 0006, second amendment, with measurements).

**Not done, and not this ticket's:** the module's own behaviour is Astra's, so the tests are of
what could go wrong on the way in rather than of every validator. Documenting `validation` for
users is part of 12.
