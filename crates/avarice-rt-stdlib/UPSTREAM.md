# Upstream

Everything under `src/components/` and `lua/` is [Astra](https://github.com/ArkForgeLabs/Astra)'s,
from version 0.51.2, commit `885586cca0ef065ac80d6a7c702d05e60fbdbb47`, at the same relative path
(Astra's `astra/lua/` is `lua/` here). `LICENSE` is the canonical Apache-2.0 text rather than
Astra's copy, which differs from it in section 8 and in the appendix.

The copyright line in `NOTICE` and in each header, `Copyright 2024 ArkForge LLC`, is the one
Astra's `LICENSE` appendix carries (`Copyright [2024] [ArkForge LLC]`, with the template brackets
left in). Astra's source files and `Cargo.toml` state no copyright holder of their own.

The rule is that these files stay Astra's. A file that is byte-for-byte Astra's has no header, so
that it stays that way. A file that has to differ opens with a header naming its Astra file and
listing every change, and the only changes made are **removals** of what is not taken and the
smallest respelling needed to compile against a different mlua. Nothing has been altered in how
anything Astra does works. The one thing done differently is how `validation.lua` is *loaded*,
which is this crate's code and not Astra's: see "How `validation` is loaded" below.

The files were committed unedited first (`git show 9260d8d`), so `git diff 9260d8d -- <file>` is
exactly what was changed in that file, whatever its header says.

## Files

| File here | Astra file | Status | SHA-256 of the Astra file |
| --- | --- | --- | --- |
| `lua/crypto.lua` | `astra/lua/crypto.lua` | identical | `aaecbf3b629732f4fcba1cad54b52feffb46ecc5126df1b937a5dae9f41337d0` |
| `lua/datetime.lua` | `astra/lua/datetime.lua` | identical | `5a82eb0ea5063f6a136dc371245409c6422677d457baaa991d515e98f10b39a7` |
| `lua/fs.lua` | `astra/lua/fs.lua` | identical | `5c8aac6546925c16794dc11eebcc245f963b64920697ead2b660f43d91f0159b` |
| `lua/http.lua` | `astra/lua/http.lua` | modified | `c0bc3d0b4492887c9b9603e194fce36de60547b347e98f6593208a49a85667e2` |
| `lua/serde.lua` | `astra/lua/serde.lua` | identical | `e1d023731ee707a7d1e80909ce01ee67d47a74930ad80c46dbf85baa3d6f9bf1` |
| `lua/stores.lua` | `astra/lua/stores.lua` | identical | `28108eb1c9bebd2de54189ee82302ef5aca9f747f35b4379f0a6727c00564cd8` |
| `lua/utils.lua` | `astra/lua/utils.lua` | modified | `61801211213b7b56062c3eece7fc6e71db3b09cc05f2bbfceb5286394b5fa806` |
| `lua/validation.lua` | `astra/lua/validation.lua` | identical | `42329fab7097c6a7292a47a529164a9b448938295bcf0fe63f7b76fe35e5bf4f` |
| `src/components/astra_serde.rs` | `src/components/astra_serde.rs` | modified | `0132e3242f5bb70fef0a830b84d6feb1f69a3355772383e443e37e99f8d4c31a` |
| `src/components/crypto.rs` | `src/components/crypto.rs` | identical | `3556a1926310f4ad8674919e607aa119986457f919cd82c8f6f3e3902d73966c` |
| `src/components/datetime.rs` | `src/components/datetime.rs` | identical | `18de84ed88c4534c7a0c73b650071593db4be7a7a374fdf0a5eea0f1ea5eddf4` |
| `src/components/file_system.rs` | `src/components/file_system.rs` | modified | `30a92952b4d0a9858edc96b349820a3c124d7518a666116755eb44a204fc96fd` |
| `src/components/http/client/mod.rs` | `src/components/http/client/mod.rs` | modified | `e17cc98e291908c74e8027ebc720bf65f15a14044f34242eed3da41eef84a629` |
| `src/components/http/client/request.rs` | `src/components/http/client/request.rs` | identical | `73dd026ac3bea057625666cc35cb5ff29985e9f474e2b00d8e5aac7dd61ccfe8` |
| `src/components/http/client/userdata.rs` | `src/components/http/client/userdata.rs` | modified | `2995caecf3a318891fcb99aa6deda4d825ae22f1f2e4f6311a245300fa168f60` |
| `src/components/http/mod.rs` | `src/components/http/mod.rs` | modified | `7184a01dc5c889b0ccc652ca1731c9706ce57e22461906d1ccbc0640ff3244b3` |
| `src/components/mod.rs` | `src/components/mod.rs` | modified | `43b890e20ea4254f2996fcde0f7c32ace834b68875c011cdc294c524ec7accf3` |
| `src/components/utils.rs` | `src/components/utils.rs` | modified | `ef1f521a52200bdd992248662ec6ce1ff961d79b426c5aecc0cd2e8e6db05d1b` |

To check a file marked identical against an Astra checkout at that commit:

```sh
diff -q crates/avarice-rt-stdlib/src/components/crypto.rs ~/Astra/src/components/crypto.rs
```

## Not taken

Astra components not present here, and why:

- `src/components/http/server/`, `templates.rs`, `database.rs`, `import.rs` and the Lua layers
  `templates.lua`, `database.lua` and `test.lua`: the HTTP server, templating, the database,
  Astra's own `require` and its test harness. The spec's list of what is out of scope.
- `src/components/http/client/websocket.rs`: the WebSocket client. `AstraWebSocket` does not
  satisfy mlua 0.12's `Sync` bound on userdata under the `send` feature, which `utils.rs` needs.
- Astra's `main.rs`, `commands/` and `build.rs`: the `astra` program itself.

## Where the Astra sources meet this crate

`src/lib.rs` and `src/modules.rs` are ours and carry no header. `modules::load` does per module
what Astra's `register_components` and `import` do between them: call the module's
`register_to_lua`, then run its Lua file, which reads the primitives off the Lua globals under
their `astra_internal__` names. Those names are Astra's and are not renamed.

## How `validation` is loaded

Two things `modules::load` does for `validation`, neither of which touches Astra's file. Its regex
primitive is registered by Astra's `utils` Rust half, alongside the tasks, so `load` registers it
for `validation` as well and the module does not depend on `utils` having been built first. And
the file defines `number`, `struct`, `regex` and a dozen more as *global* functions, which Astra
gets away with because a program using it means to have them; here they would appear in every
program's globals as soon as anything required the module. `load` therefore runs it against a
table of its own that reads through to the real globals, so what the file defines stays inside
it. This is the one place a stdlib module is loaded differently from Astra's own way of loading it,
and it is a change to where the file's definitions go, not to what they do.
