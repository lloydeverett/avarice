# 12: README and crate documentation

**What to build:** a reader who has never seen this project can find out what
the stdlib offers, that trusted mode registers all of it, and how to hand out
most of it without handing out `http`.

Each earlier ticket carries its own documentation. This one is what only makes
sense once all seven modules exist: the README section that describes them as a
set, and the crate-level docs that describe the runtime's new shape.

**Blocked by:** 05, 06, 07, 08, 09, 10 (all seven modules).

**Status:** ready-for-agent

- [ ] A README section covering the seven modules, what each is for, and the selection API.
- [ ] The crate-level documentation covers the async entry points and the write sink.
- [ ] `Profile::Trusted`'s documentation says it registers stdlib modules, and that trusted is not harmless: the stdlib reaches the network and the filesystem.
- [ ] An embedder already inside a tokio runtime is told to build the runtime on its own thread, so they find that out from the docs rather than from a panic.
- [ ] The vocabulary matches the glossary — **stdlib module**, **task**, **write sink**, **profile**, **host module**, **module store**, **embedder** — rather than inventing synonyms for terms already defined.
