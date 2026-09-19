# 05: `crypto`

**What to build:** `require("crypto")` — hashing, HMAC, base64 and UUIDs, so a
script can sign a request, verify a payload, carry binary through a text
protocol, or label something uniquely without inventing a scheme.

This is the first module with a Rust half, so it is where the derived-file
conventions get settled against something real rather than agreed in the
abstract.

**Blocked by:** 04 (module selection).

**Status:** ready-for-agent

- [ ] SHA-2 and SHA-3 families, HMAC, base64 encode and decode, and UUID generation, all reachable through `require`.
- [ ] Hashes are checked against published test vectors, never against our own output.
- [ ] Base64 round-trips, including input that is not valid UTF-8.
- [ ] Decoding malformed base64 raises a Lua error a script can `pcall`.
- [ ] **Settles the attribution header** for every derived file that follows: the Astra file it came from, the copyright and licence line, a pointer to `LICENSE` and `NOTICE`, and a `Changes from the original:` list that is mandatory and never empty — a file copied verbatim says so on that line.
- [ ] **Settles the two-layer shape**: Rust provides the primitives, an embedded Lua file wraps them into the module table, and the Lua source is embedded directly in the binary with no build script and no precompilation step.
- [ ] The internal globals the Rust half sets are renamed from Astra's `astra_internal__` prefix to ours. They are ours now, and the old prefix would claim a project name that is not. That rename is the first `Changes from the original:` line in every derived file that has a Lua layer.
- [ ] Because modules register lazily, a module's internal globals appear on first `require` of that module and not before.
