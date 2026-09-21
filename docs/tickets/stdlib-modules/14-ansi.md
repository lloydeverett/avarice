# 14: `ansi`

> **Amended 2026-09-21.** `ansi` is no longer a stdlib module. It is a **core module**, registered
> in every runtime with no feature, so `StdModule::Ansi`, `StdModules::ANSI`, `stdlib-ansi` and its
> place in `stdlib()` below are gone, and the count is eight again. What the ticket says about the
> module's contents stands. See
> [ADR 0011](../../adr/0011-print-highlights-and-ansi-is-a-core-module.md).

**What to build:** `require("ansi")`, a table of ANSI escape codes for formatting text on a
terminal, and the few functions a table cannot hold, as the ninth **stdlib module**. It is written
in Lua only, so it is the first **pure module** a sandbox registers, and the first that is not
Astra's. See [ADR 0008](../../adr/0008-ansi-is-original-and-pure.md), and
[ADR 0007](../../adr/0007-stdlib-modules-are-compile-time-optional.md) for why sandbox mode now
registers the pure modules.

**Blocked by:** 04 (module selection).

**Status:** done.

- [x] `StdModule::Ansi`, `StdModules::ANSI` at `1 << 8`, the name `ansi` and the feature `stdlib-ansi`, in `stdlib` and in `default`, forwarded by `avarice-rt`. `StdModules` is `u16`, and every existing bit keeps its value.
- [x] Purity is recorded once per module, in the stdlib crate's table, and `StdModules::PURE` is derived from it. `Profile::Sandbox` starts from `PURE`, so a sandbox has `stores` and `ansi` and no other module. `stdlib()` lists them.
- [x] A sandbox test builds every pure module and finds no `astra_internal__*` global afterwards, which is the observable meaning of "no Rust behind it".
- [x] `lua/ansi.lua` opens with a header saying it is original to avarice-rt, is pure, and is Apache-2.0 like the crate. It has no `Changes from the original:` list.
- [x] Styles are flat, complete escape sequences: `reset`, `bold`, `dim`, `italic`, `underline`, `blink`, `reverse`, `hidden`, `strikethrough`, and a `no_` off code for each but `reset`. `no_bold` and `no_dim` are both `\27[22m`, because the terminal has one code for both.
- [x] `ansi.fg` and `ansi.bg` hold the eight named colours, their `bright_` variants and `default` (`30`–`37`, `90`–`97`, `39` for foreground; `40`–`47`, `100`–`107`, `49` for background).
- [x] `fg` and `bg` also hold `rgb(r, g, b)`, `color256(n)` and `hex(s)`. `rgb` and `color256` raise `rgb: red component must be an integer 0–255` and the like, blamed on the caller, for anything that is not one: out of range, fractional, a string, missing. A float with an integer value, such as `255.0`, is accepted. `hex` takes six hex digits with or without a `#`, in either case; the three-digit form is refused.
- [x] Unknown names are `nil`, and the tables are plain tables: no read-only mechanism, and no error on a missing key.
- [x] The module does not look at a TTY or `NO_COLOR`; the codes are always real ones.
- [x] `stores` and `ansi` are documented as pure in the stdlib crate's README, on `StdModule`, and in the README's stdlib section. Counts of modules say nine, and `scripts/check-features.sh` builds `ansi` alone.

**Out of scope, and not to be added without a decision:** cursor movement, erasing and screen
control, hyperlinks, and a `paint(text, ...)` wrapper that closes what it opens.
