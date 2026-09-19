# 07: `datetime`

**What to build:** `require("datetime")` — instants, civil dates and times,
zones, spans and formatting, so a script can timestamp work, schedule it, read
what another system produced and produce what another system expects.

This is the one clean-room module. Astra's is written directly against chrono's
types, and porting it would have produced a rewrite wearing a copy's
attribution. It is written from scratch against jiff, on jiff's own model.

**Blocked by:** 04 (module selection).

**Status:** ready-for-agent

Last of the module tickets by preference rather than by dependency: it shares
nothing with its neighbours, so it is the one that benefits least from their
conventions and disturbs them least.

- [ ] Six distinct Lua userdata types: zoned datetime, timestamp, civil datetime, civil date, civil time, span, and timezone.
- [ ] `SignedDuration` is deliberately not among them. It and `Span` differ in ways that matter to jiff — calendar units against absolute ones — but would read as a confusing pair of near-identical Lua types. `Span` covers what scripts need.
- [ ] Metamethods throughout: `__tostring`, `__eq`, `__lt`, `__le`, and `__add`/`__sub` taking a span, so `now + span` reads as arithmetic rather than as a method call.
- [ ] A span constructor taking named units replaces Astra's sixteen `add_*`/`sub_*` methods.
- [ ] Per-type constructors, **not** Astra's single overloaded one that switches on the type of its first argument. `new(2024)` meaning "the year 2024" while `new("2024")` means "parse this" is a trap, and there is no copy-fidelity reason to carry it into a file being written from scratch.
- [ ] Astra's three `to_locale_*` methods are not carried over: jiff has no locale formatting, and chrono only does it behind an unstable feature.
- [ ] Parse and format round-trip. Two instants compare correctly. Arithmetic across a daylight-saving boundary does what the zone says, not what arithmetic on a number would say.
- [ ] jiff's timezone database is bundled into the binary, so `datetime` works on a distroless or scratch container where there is no system zoneinfo to read.
- [ ] **No Astra attribution header on this file.** The crate README says why this one differs from its neighbours, so the inconsistency reads as a decision rather than an oversight.
