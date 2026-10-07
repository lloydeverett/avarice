# Examples

Two crates that use avarice the way crates outside this repository would, through its public API
and nothing else.

- **[`greeting`](greeting)** contributes a module (ADR 0020). It implements `HostModule` for
  `Greeting`, a module with a Rust half and a Lua half, and depends on nothing but avarice: the
  `mlua` it uses is `avarice::mlua`.
- **[`app`](app)** is an embedder. It builds a sandboxed runtime, chooses a stdlib module, adds
  `greeting`, sets limits and a module store, registers a module of its own, and runs the Lua
  program in [`app/lua`](app/lua).

```sh
cargo run -p example-app
cargo test --workspace     # avarice's tests, and the examples'
```
