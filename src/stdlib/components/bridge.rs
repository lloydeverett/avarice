//! `bridge` is original to avarice, not derived from Astra: ADR 0022 records why. It gives Lua a
//! **Bridge table**, a value a script writes and reads as a Lua table, which the host can watch.
//!
//! A Bridge table is a userdata that is a thin handle on shared state, which holds its contents in
//! an ordinary Lua table, its backing table. Its metamethods reach the backing table and nothing
//! else, and it has no methods, so every key is the script's: whatever is done to one besides
//! reading and writing it is a function of `bridge.table`, taking it first. `__pairs` and `__len`
//! make `pairs`, `ipairs`, `#` and the `table` library work on it as on a table.
//!
//! Nothing here sets a global. [`rust_half`] builds the module's table, and `modules.rs` hands it
//! to `lua/bridge.lua`, which returns it.

use std::sync::Arc;

use mlua::{AnyUserData, Function, Lua, MetaMethod, MultiValue, Table, UserData, Value};

type Result<T> = mlua::Result<T>;

/// The name a Bridge table gives itself, as its `__name` and in `tostring`.
const TABLE_NAME: &str = "bridge.table";

/// Where `rust_half` keeps Lua's `next`, for `__pairs` to give. Taken when the module is built, so
/// that a script that later replaces the global `next` does not change how a Bridge table walks.
const NEXT: &str = "avarice.bridge.next";

/// Builds the module's table: `bridge.table`, a table whose `__call` makes a Bridge table.
pub fn rust_half(lua: &Lua) -> Result<Value> {
    let next: Function = lua.globals().raw_get("next")?;
    lua.set_named_registry_value(NEXT, next)?;

    let table = lua.create_table()?;
    let make = lua.create_table()?;
    make.set(
        "__call",
        lua.create_function(|lua, mut values: MultiValue| {
            // The first is `bridge.table` itself.
            values.pop_front();
            let [init] = args(values, "bridge.table")?;
            make_table(lua, init)
        })?,
    )?;
    table.set_metatable(Some(make))?;

    let module = lua.create_table()?;
    module.set("table", table)?;
    Ok(Value::Table(module))
}

/// `bridge.table(init?)`: a new Bridge table, holding a shallow copy of `init` if it is given.
fn make_table(lua: &Lua, init: Value) -> Result<AnyUserData> {
    let backing = lua.create_table()?;
    match init {
        Value::Nil => {}
        Value::Table(init) => init.for_each(|k: Value, v: Value| backing.raw_set(k, v))?,
        other => {
            return Err(error(format!(
                "bridge.table: expected a table for init, got {}",
                other.type_name()
            )));
        }
    }
    let state = Arc::new(TableState { backing });
    lua.create_userdata(BridgeTable(state))
}

/// What a Bridge table is: its backing table. The userdata, and later the host's handles, are
/// handles on this.
struct TableState {
    backing: Table,
}

/// The userdata Lua holds a Bridge table as.
struct BridgeTable(Arc<TableState>);

impl UserData for BridgeTable {
    fn add_fields<F: mlua::UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field(MetaMethod::Type, TABLE_NAME);
    }

    fn add_methods<M: mlua::UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(MetaMethod::Index, |_, this, key: Value| {
            this.0.backing.raw_get::<Value>(key)
        });
        methods.add_meta_method(
            MetaMethod::NewIndex,
            |_, this, (key, value): (Value, Value)| {
                // As a table would. mlua leaves these to Lua, which in a build with its API checks
                // on asserts rather than raising.
                match key {
                    Value::Nil => Err(error("index is nil".to_owned())),
                    Value::Number(n) if n.is_nan() => Err(error("index is NaN".to_owned())),
                    key => this.0.backing.raw_set(key, value),
                }
            },
        );
        methods.add_meta_method(MetaMethod::Len, |_, this, ()| Ok(this.0.backing.raw_len()));
        methods.add_meta_method(MetaMethod::Pairs, |lua, this, ()| {
            let next: Function = lua.named_registry_value(NEXT)?;
            Ok((next, this.0.backing.clone(), Value::Nil))
        });
        methods.add_meta_function(MetaMethod::ToString, |_, this: AnyUserData| {
            Ok(format!(
                "{TABLE_NAME}: {:p}",
                Value::UserData(this).to_pointer()
            ))
        });
    }
}

/// The values a function was called with, as exactly `N`, nil standing for one not given. More
/// than `N` raises, where Lua would drop the extra ones, as in `tidings` and `datetime`.
fn args<const N: usize>(values: MultiValue, what: &str) -> Result<[Value; N]> {
    if values.len() > N {
        let expected = match N {
            0 => "no arguments".to_owned(),
            1 => "at most 1 argument".to_owned(),
            n => format!("at most {n} arguments"),
        };
        return Err(error(format!(
            "{what}: expected {expected}, got {}",
            values.len()
        )));
    }
    let mut values = values.into_iter();
    Ok(std::array::from_fn(|_| values.next().unwrap_or(Value::Nil)))
}

fn error(message: String) -> mlua::Error {
    mlua::Error::runtime(message)
}
