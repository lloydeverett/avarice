//! `dirs` is original to avarice-rt, not derived from Astra: ADR 0014 records why. It wraps
//! `etcetera`'s directory resolution, registering one primitive per directory kind. Each takes the
//! application's identity — name, author, top-level domain — and resolves it against the host's
//! strategy (`Xdg` on Linux and macOS, `Windows` elsewhere). `config`, `data` and `cache` always
//! resolve; `state` and `runtime` can come back `nil`, since not every strategy supports them.
//!
//! No path returned here is created. Resolving one is not I/O; the `fs` module is what writes to
//! it.

use etcetera::app_strategy::{AppStrategy, AppStrategyArgs};
use etcetera::choose_app_strategy;

pub fn register_to_lua(lua: &mlua::Lua) -> mlua::Result<()> {
    lua.globals()
        .set("astra_internal__dirs_config", lua.create_function(config)?)?;
    lua.globals()
        .set("astra_internal__dirs_data", lua.create_function(data)?)?;
    lua.globals()
        .set("astra_internal__dirs_cache", lua.create_function(cache)?)?;
    lua.globals()
        .set("astra_internal__dirs_state", lua.create_function(state)?)?;
    lua.globals().set(
        "astra_internal__dirs_runtime",
        lua.create_function(runtime)?,
    )?;
    Ok(())
}

/// `(app_name, author, top_level_domain)`, as `dirs.lua`'s `App` passes it on every call.
type Identity = (String, String, String);

fn strategy(
    (app_name, author, top_level_domain): Identity,
) -> mlua::Result<impl AppStrategy> {
    choose_app_strategy(AppStrategyArgs {
        top_level_domain,
        author,
        app_name,
    })
    .map_err(|e| mlua::Error::runtime(format!("could not determine the home directory: {e}")))
}

fn to_lua_path(path: std::path::PathBuf) -> String {
    path.to_string_lossy().into_owned()
}

fn config(_: &mlua::Lua, identity: Identity) -> mlua::Result<String> {
    Ok(to_lua_path(strategy(identity)?.config_dir()))
}

fn data(_: &mlua::Lua, identity: Identity) -> mlua::Result<String> {
    Ok(to_lua_path(strategy(identity)?.data_dir()))
}

fn cache(_: &mlua::Lua, identity: Identity) -> mlua::Result<String> {
    Ok(to_lua_path(strategy(identity)?.cache_dir()))
}

fn state(_: &mlua::Lua, identity: Identity) -> mlua::Result<Option<String>> {
    Ok(strategy(identity)?.state_dir().map(to_lua_path))
}

fn runtime(_: &mlua::Lua, identity: Identity) -> mlua::Result<Option<String>> {
    Ok(strategy(identity)?.runtime_dir().map(to_lua_path))
}
