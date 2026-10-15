//! `bridge` is original to avarice, not derived from Astra: ADR 0022 records why. It gives Lua a
//! **Bridge table**, a value a script writes and reads as a Lua table, which the host can watch.
//!
//! A Bridge table is a userdata whose contents are an ordinary Lua table, its backing table. Its
//! metamethods reach the backing table and nothing else, and it has no methods, so every key is
//! the script's: whatever is done to one besides reading and writing it is a function of
//! `bridge.table`, taking it first. `__pairs` and `__len` make `pairs`, `ipairs`, `#` and the
//! `table` library work on it as on a table, and the backing table itself never reaches a script,
//! which could otherwise write past the Bridge table.
//!
//! Where each part lives is decided by the garbage collector. ADR 0022 has Lua own a Bridge table,
//! collected once Lua holds no reference to it, and a script will store one in itself, or in a
//! closure stored in itself. So whatever is Lua's lives in Lua, as the userdata's user values,
//! which the collector traces: the backing table now, and Lua's subscribers when there are any. A
//! Lua value held from Rust is a registry reference, which the collector treats as a root, so a
//! Bridge table holding itself through one would never be collected. Rust holds only what is not
//! Lua's, in [`TableState`]. The host's handle on a Bridge table must likewise be weak: something
//! like an id it looks up in a weak-valued table, through a `WeakLua`, and never an `AnyUserData`
//! or an `mlua::Table`, which are registry references too.
//!
//! Nothing here sets a global. [`rust_half`] builds the module's table, and `modules.rs` hands it
//! to `lua/bridge.lua`, which returns it.

use std::sync::Arc;

use mlua::{AnyUserData, Function, Lua, MetaMethod, MultiValue, Table, UserData, Value};

type Result<T> = mlua::Result<T>;

/// The name a Bridge table gives itself, as its `__name` and in `tostring`.
const TABLE_NAME: &str = "bridge.table";

/// Where `rust_half` keeps the iterator `__pairs` gives, which walks a Bridge table's backing table
/// with the `next` there was when the module was built, so that a script that later replaces the
/// global `next` does not change how a Bridge table walks.
const PAIRS_ITERATOR: &str = "avarice.bridge.pairs";

/// The user value of a Bridge table's userdata that holds its backing table.
const BACKING: usize = 1;

/// Builds the module's table: `bridge.table`, a table whose `__call` makes a Bridge table.
pub fn rust_half(lua: &Lua) -> Result<Value> {
    let next: Function = lua.globals().raw_get("next")?;
    // `pairs` passes it the userdata as its state, so it is all a script is given.
    let iterator = lua.create_function(move |_, (this, key): (AnyUserData, Value)| {
        next.call::<MultiValue>((backing(&this)?, key))
    })?;
    lua.set_named_registry_value(PAIRS_ITERATOR, iterator)?;

    let table_fn = lua.create_table()?;
    let table_meta = lua.create_table()?;
    table_meta.set(
        "__call",
        lua.create_function(|lua, mut values: MultiValue| {
            // The first is `bridge.table` itself.
            values.pop_front();
            let [init] = args(values, "bridge.table")?;
            make_table(lua, init)
        })?,
    )?;
    table_fn.set_metatable(Some(table_meta))?;

    let module = lua.create_table()?;
    module.set("table", table_fn)?;
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
    let this = lua.create_userdata(BridgeTable(Arc::new(TableState {})))?;
    this.set_nth_user_value(BACKING, backing)?;
    Ok(this)
}

/// The backing table of `this`, which must be a Bridge table's userdata.
fn backing(this: &AnyUserData) -> Result<Table> {
    if !this.is::<BridgeTable>() {
        return Err(error(format!("expected a {TABLE_NAME}")));
    }
    this.nth_user_value(BACKING)
}

/// The part of a Bridge table that is not Lua's, shared between its userdata and, later, the
/// host's handles. It holds no Lua value, for the reasons the module's header gives: the backing
/// table is a user value of the userdata. It will hold whether the table is closed, the Rust
/// subscribers and the batch depth.
struct TableState {}

/// The userdata Lua holds a Bridge table as. Its first user value is the backing table.
// Nothing reads the state until there is something in it: subscribing and closing (tickets 03 and
// 05).
struct BridgeTable(#[allow(dead_code)] Arc<TableState>);

impl UserData for BridgeTable {
    fn add_fields<F: mlua::UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field(MetaMethod::Type, TABLE_NAME);
    }

    fn add_methods<M: mlua::UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_function(MetaMethod::Index, |_, (this, key): (AnyUserData, Value)| {
            backing(&this)?.raw_get::<Value>(key)
        });
        methods.add_meta_function(
            MetaMethod::NewIndex,
            |_, (this, key, value): (AnyUserData, Value, Value)| {
                // As a table would. mlua leaves these to Lua, which in a build with its API checks
                // on asserts rather than raising.
                match key {
                    Value::Nil => Err(error("index is nil".to_owned())),
                    Value::Number(n) if n.is_nan() => Err(error("index is NaN".to_owned())),
                    key => backing(&this)?.raw_set(key, value),
                }
            },
        );
        methods.add_meta_function(MetaMethod::Len, |_, this: AnyUserData| {
            Ok(backing(&this)?.raw_len())
        });
        methods.add_meta_function(MetaMethod::Pairs, |lua, this: AnyUserData| {
            let iterator: Function = lua.named_registry_value(PAIRS_ITERATOR)?;
            Ok((iterator, this, Value::Nil))
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
