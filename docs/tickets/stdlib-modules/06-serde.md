# 06: `serde`

**What to build:** `require("serde")` — JSON encoding and decoding, so a script
can exchange structured data with anything.

The module is called `serde` even though it offers one format. Astra does not
get to define what the word means by having offered eight.

**Blocked by:** 04 (module selection).

**Status:** ready-for-agent

- [ ] Encode a Lua table to JSON and decode JSON into a Lua table, reachable through `require`.
- [ ] Round-trips hold for the shapes Lua actually produces: nested tables, arrays, mixed keys, numbers, booleans, and nil handling that is documented rather than incidental.
- [ ] Decoding malformed JSON raises a Lua error a script can `pcall`, so a bad payload does not take the program down.
- [ ] Carries the attribution header settled in 05, with its own `Changes from the original:` list — which includes the removal of the seven non-JSON formats Astra's module offers.
