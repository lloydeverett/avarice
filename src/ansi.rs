//! `ansi`, the one **core module**: a table of ANSI escape codes (`ansi.lua`).
//!
//! The core registers it in every runtime, whatever the profile, because `print` highlights with
//! these codes (ADR 0011). It is Lua and nothing else, so it is pure in the sense ADR 0007 gives
//! the word, and every limit a runtime puts on Lua governs it.

use mlua::chunk::ChunkMode;
use mlua::{Lua, Table};

const SOURCE: &str = include_str!("lua/ansi.lua");

/// Evaluates `ansi.lua`, and returns the table it builds.
///
/// Every call builds a table of its own. `print` is handed one, and `require("ansi")` builds
/// another, so a script that edits the table it was given cannot change what `print` emits.
pub(crate) fn build(lua: &Lua) -> mlua::Result<Table> {
    lua.load(SOURCE)
        .set_name("=[avarice ansi]")
        .set_mode(ChunkMode::Text)
        .eval()
}
