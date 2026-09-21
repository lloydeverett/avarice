# 12: README and crate documentation

**What to build:** a reader who has never seen this project can find out what
the stdlib offers, that trusted mode registers all of it, and how to hand out
most of it without handing out `http`.

Each earlier ticket carries its own documentation. This one is what only makes
sense once all eight modules exist: the README section that describes them as a
set, and the crate-level docs that describe the runtime's new shape.

**Blocked by:** 05, 06, 07, 08, 09, 10, 13 (all eight modules).

**Status:** ready-for-agent

- [ ] A README section covering the eight modules, what each is for, and the selection API.
- [ ] `validation` is documented for a user, not just listed: `require("validation").types` and its validators with the messages they report, `.regex` with `is_match`, `captures` and `replace`, and the warning that the Rust behind the regex is outside the sandbox's memory and time limits (see the second amendment to ADR 0006).
- [ ] The crate-level documentation covers the async entry points and the write sink.
- [ ] `Profile::Trusted`'s documentation says it registers stdlib modules, and that trusted is not harmless: the stdlib reaches the network and the filesystem.
- [ ] An embedder already inside a tokio runtime is told to build the runtime on its own thread, so they find that out from the docs rather than from a panic.
- [ ] The vocabulary matches the glossary — **stdlib module**, **task**, **write sink**, **profile**, **host module**, **module store**, **embedder** — rather than inventing synonyms for terms already defined.
