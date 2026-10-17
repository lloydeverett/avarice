//! Bundled modules (ADR 0026): modules avarice ships that are plain Lua, built on what a runtime
//! registers, and that no profile registers. An embedder adds one with
//! [`RuntimeBuilder::module`](crate::RuntimeBuilder::module), as it would a contributed module,
//! and each is behind a Cargo feature of its own, `bundled-<name>`.
//!
//! A bundled module has no Rust half and no privilege of its own: it reaches what the runtime it
//! is added to gives every script, and raises from `require` when that is not enough.

use mlua::{Lua, Value};

use crate::HostModule;

/// `openai`, a client for OpenAI-compatible inference servers, with streamed chat completions.
///
/// It needs `http` and `serde` registered and the `os` library open, as trusted mode has them,
/// and reads `OPENAI_BASE_URL`, `OPENAI_API_KEY` and `OPENAI_MODEL` for what a script leaves out.
///
/// ```
/// use avarice::{Profile, Runtime, bundled::OpenAi};
///
/// let rt = Runtime::builder(Profile::Trusted).module(OpenAi).build()?;
/// rt.block_on(rt.exec("local openai = require('openai')", "=example"))?;
/// # Ok::<_, avarice::Error>(())
/// ```
#[derive(Debug, Clone, Copy, Default)]
pub struct OpenAi;

impl HostModule for OpenAi {
    fn name(&self) -> &str {
        "openai"
    }

    fn load(&self, lua: &Lua) -> mlua::Result<Value> {
        // Named so a traceback through it says where it came from.
        lua.load(include_str!("lua/openai.lua"))
            .set_name("=[avarice bundled openai]")
            .set_mode(mlua::chunk::ChunkMode::Text)
            .eval()
    }
}
