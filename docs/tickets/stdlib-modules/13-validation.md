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
- [x] `lua/validation.lua` is byte-identical to Astra's, carries no header, and is listed in `UPSTREAM.md` with its checksum.
- [x] Regular expressions work: `is_match`, `captures`, `replace` with and without a limit, and a bad pattern is an error a script can catch.
- [x] They work when `validation` is the only module selected, so the module does not lean on `utils` having been built.
- [x] Astra's validators load and run under this runtime: struct, array, union, literal, range, pattern, optional and `build` with defaults, including the error paths they report.
- [x] Requiring the module adds no globals, and a script reusing a name the file defines, `number` or `struct`, neither breaks it nor is broken by it. Astra's file defines them as globals; `modules::load` runs it against a table of its own.
- [x] The docs that count the modules say eight.

**Not done, and not this ticket's:** the module's own behaviour is Astra's, so the tests are of
what could go wrong on the way in rather than of every validator. Documenting `validation` for
users is part of 12.
