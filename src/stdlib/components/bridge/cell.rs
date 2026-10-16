//! A **Bridge cell**: one value a script holds, replaced whole, which the host can watch.
//!
//! A Bridge cell is a userdata whose value is its user value [`VALUE`]; the part that is not Lua's
//! is a [`Core`]. It has no keys for a method to hide, so its operations are methods, found
//! through an `__index` that is a table of them, kept in the registry at [`METHODS`].

use std::fmt;
use std::sync::{Arc, Weak};

use mlua::{
    AnyUserData, FromLua, Function, IntoLua, IntoLuaMulti, Lua, MetaMethod, MultiValue,
    Table as LuaTable, UserData, Value,
};

use serde::{Serialize, Serializer};

use super::shared::{self, Core, Handle, Kind, OnClose, Subscription};
use super::{Helpers, Result, args, error};

/// What a Bridge cell is called, as its `__name` and in `tostring`, and in prose.
static CELL: Kind = Kind {
    name: "bridge.cell",
    noun: "Bridge cell",
};

/// Where [`build`] keeps the table of a Bridge cell's methods, its `__index`.
const METHODS: &str = "avarice.bridge.cell.methods";

/// Where [`build`] keeps the functions through which a [`Cell`] gets, sets and closes, called
/// into Lua as a [`Table`](super::Table)'s are, and for the same reasons.
const HOST_GET: &str = "avarice.bridge.cell.get";
const HOST_SET: &str = "avarice.bridge.cell.set";
const HOST_CLOSE: &str = "avarice.bridge.cell.close";

/// The user value of a Bridge cell's userdata that holds its value.
const VALUE: usize = 1;

/// Builds `bridge.cell`, which makes a Bridge cell, and the methods of one.
pub(super) fn build(lua: &Lua, helpers: &Helpers) -> Result<Function> {
    let get = lua.create_function(|lua, id: u64| {
        shared::handled(lua, id, &CELL)?.nth_user_value::<Value>(VALUE)
    })?;
    lua.set_named_registry_value(HOST_GET, get)?;
    let host_set = lua.create_function(|lua, (id, value): (u64, Value)| {
        set(lua, &shared::handled(lua, id, &CELL)?, value, CELL.name)
    })?;
    lua.set_named_registry_value(HOST_SET, host_set)?;
    let host_close =
        lua.create_function(|lua, id: u64| close(&shared::handled(lua, id, &CELL)?))?;
    lua.set_named_registry_value(HOST_CLOSE, host_close)?;

    let methods = lua.create_table()?;
    methods.set(
        "get",
        lua.create_function(|_, mut values: MultiValue| {
            const WHAT: &str = "bridge.cell.get";
            let this = bridge_cell(values.pop_front().unwrap_or(Value::Nil), WHAT)?;
            let [] = args(values, WHAT)?;
            this.nth_user_value::<Value>(VALUE)
        })?,
    )?;
    methods.set(
        "set",
        lua.create_function(|lua, mut values: MultiValue| {
            const WHAT: &str = "bridge.cell.set";
            let this = bridge_cell(values.pop_front().unwrap_or(Value::Nil), WHAT)?;
            let [value] = args(values, WHAT)?;
            set(lua, &this, value, WHAT)
        })?,
    )?;
    let protecting = helpers.clone();
    let update = lua.create_function(move |lua, mut values: MultiValue| {
        const WHAT: &str = "bridge.cell.update";
        let this = open_cell(values.pop_front().unwrap_or(Value::Nil), WHAT)?;
        let [body] = args(values, WHAT)?;
        let Value::Function(body) = body else {
            return Err(error(format!(
                "{WHAT}: expected a function, got {}",
                body.type_name()
            )));
        };
        let old: Value = this.nth_user_value(VALUE)?;
        match protecting.protect(lua, WHAT, body, old)? {
            Ok(new) => set(lua, &this, new, WHAT).map(|()| (true, Value::Nil)),
            Err(raised) => Ok((false, raised)),
        }
    })?;
    methods.set("update", helpers.raising.call::<Function>(update)?)?;
    methods.set(
        "subscribe",
        lua.create_function(|lua, mut values: MultiValue| {
            const WHAT: &str = "bridge.cell.subscribe";
            let this = bridge_cell(values.pop_front().unwrap_or(Value::Nil), WHAT)?;
            let [subscriber, on_close] = args(values, WHAT)?;
            let subscriber = shared::subscriber_args(subscriber, on_close, WHAT)?;
            let state = state_of(&this)?;
            shared::subscribe_lua(lua, this, state, |state| state, subscriber, WHAT)
        })?,
    )?;
    methods.set(
        "close",
        lua.create_function(|_, mut values: MultiValue| {
            const WHAT: &str = "bridge.cell.close";
            let this = bridge_cell(values.pop_front().unwrap_or(Value::Nil), WHAT)?;
            let [] = args(values, WHAT)?;
            close(&this)
        })?,
    )?;
    lua.set_named_registry_value(METHODS, methods)?;

    lua.create_function(|lua, values: MultiValue| {
        let [value] = args(values, CELL.name)?;
        let this = lua.create_ser_userdata(BridgeCell(Arc::new(Core::new(lua, &CELL))))?;
        shared::enter(lua, &this, &*state_of(&this)?)?;
        this.set_nth_user_value(VALUE, value)?;
        Ok(this)
    })
}

/// `value` as a Bridge cell's userdata, or `value` back if it is not one. Everything that takes a
/// Bridge cell from Lua comes through here.
fn as_bridge_cell(value: Value) -> std::result::Result<AnyUserData, Value> {
    match value {
        Value::UserData(this) if this.is::<BridgeCell>() => Ok(this),
        other => Err(other),
    }
}

/// `value` as a Bridge cell's userdata, or an error naming `what` was given something else.
fn bridge_cell(value: Value, what: &str) -> Result<AnyUserData> {
    as_bridge_cell(value).map_err(|other| {
        error(format!(
            "{what}: expected a {}, got {}",
            CELL.name,
            other.type_name()
        ))
    })
}

/// `value` as a Bridge cell's userdata that is not closed, or an error naming `what`.
fn open_cell(value: Value, what: &str) -> Result<AnyUserData> {
    let this = bridge_cell(value, what)?;
    if state_of(&this)?.is_closed() {
        return Err(shared::is_closed(&CELL, what));
    }
    Ok(this)
}

fn state_of(this: &AnyUserData) -> Result<Arc<CellState>> {
    Ok(Arc::clone(&this.borrow::<BridgeCell>()?.0))
}

/// Sets the value of the Bridge cell `this` to `new`, from Lua or from Rust, and tells every
/// subscriber the value it had and `new`, once it is in place, even when the two are the same.
/// On one that is closed it is an error, as of `what`.
fn set(lua: &Lua, this: &AnyUserData, new: Value, what: &str) -> Result<()> {
    let state = state_of(this)?;
    if state.is_closed() {
        return Err(shared::is_closed(&CELL, what));
    }
    let old: Value = this.nth_user_value(VALUE)?;
    this.set_nth_user_value(VALUE, &new)?;
    let rust = |_, subscriber: &CellSubscriber| {
        subscriber(lua, old.clone(), new.clone());
        Ok(())
    };
    let lua_args = || (&old, &new).into_lua_multi(lua);
    shared::tell(this, &state, rust, lua_args)
}

/// `c:close()`, and [`Cell::close`]: closes `this`, as [`shared::close`] does. A cell holds
/// nothing back, so there is nothing to tell before the close.
fn close(this: &AnyUserData) -> Result<()> {
    shared::close(this, &*state_of(this)?, |_| Ok(()))
}

/// A Rust subscriber to a Bridge cell, given the value it had and the value it has now.
type CellSubscriber = dyn Fn(&Lua, Value, Value) + Send + Sync;

/// The userdata Lua holds a Bridge cell as. Its first user value is its value, its second, once
/// it has had a Lua subscriber, the Lua subscribers, and its third, once a Lua subscriber has given
/// one, the Lua close callbacks.
struct BridgeCell(Arc<CellState>);

/// The part of a Bridge cell that is not Lua's, shared between its userdata and the host's
/// handles. It holds no Lua value: the value, the Lua subscribers' functions and their close
/// callbacks are user values of the userdata.
type CellState = Core<CellSubscriber>;

impl UserData for BridgeCell {
    fn add_fields<F: mlua::UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field(MetaMethod::Type, CELL.name);
        fields.add_meta_field_with(MetaMethod::Index, |lua| {
            lua.named_registry_value::<LuaTable>(METHODS)
        });
    }

    fn add_methods<M: mlua::UserDataMethods<Self>>(methods: &mut M) {
        // What `print` shows (ADR 0023): the value, which `get` gives anyone already.
        methods.add_meta_function("__view", |_, this: Value| {
            bridge_cell(this, CELL.name)?.nth_user_value::<Value>(VALUE)
        });
        methods.add_meta_function(MetaMethod::ToString, |_, this: AnyUserData| {
            Ok(format!(
                "{}: {:p}",
                CELL.name,
                Value::UserData(this).to_pointer()
            ))
        });
    }
}

/// `serde` encodes a Bridge cell as its value.
impl Serialize for BridgeCell {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        shared::serialize(&self.0, serializer, |this| this.nth_user_value(VALUE))
    }
}

/// Collection closes a Bridge cell nobody closed, and so does dropping the runtime, as
/// [`Core::close_by_collection`] has it.
impl Drop for BridgeCell {
    fn drop(&mut self) {
        self.0.close_by_collection();
    }
}

/// The host's handle on a Bridge cell, received from Lua through [`FromLua`].
///
/// It holds the Bridge cell weakly, as a [`Table`](super::Table) holds a Bridge table: Lua owns
/// it, and collects it once no Lua value refers to it, whether or not the host still has a handle
/// (ADR 0022), and so closes it. Setting one that is closed returns an error, and so does getting
/// or setting one that has been collected, or whose `Lua` is gone.
#[derive(Clone)]
pub struct Cell {
    state: Weak<CellState>,
    handle: Handle,
}

impl Cell {
    /// The value the cell holds, as a script's `c:get()` gives it.
    pub fn get<V: FromLua>(&self) -> Result<V> {
        self.handle.call(HOST_GET, self.handle.id)
    }

    /// Sets the value the cell holds, as a script's `c:set(value)` does, and tells every
    /// subscriber, Lua's included, before it returns, even when `value` is the value it held. The
    /// set and the telling are one call into Lua, so they hold the Lua lock throughout, from
    /// whichever thread this is called.
    ///
    /// Setting one that is closed returns an error, without calling into Lua, so a close callback
    /// may try it even while the cell is being collected.
    pub fn set(&self, value: impl IntoLua) -> Result<()> {
        match self.state.upgrade() {
            Some(state) if state.is_closed() => Err(shared::is_closed(&CELL, CELL.name)),
            _ => self.handle.call(HOST_SET, (self.handle.id, value)),
        }
    }

    /// Calls `subscriber` with the value the cell held and the value it holds now, on every set
    /// from now on, until the returned [`Subscription`] is dropped.
    ///
    /// It runs as a [`Table`](super::Table)'s subscriber does: on the Lua thread, inside the set,
    /// once the value is in place, and the script that set waits for it, so one with slow work to
    /// do hands it off and returns. It may set the cell, which tells every subscriber of that set
    /// before it returns. An `mlua::Value` of the cell that it keeps keeps the cell alive.
    ///
    /// Subscribing to one that is closed, or has been collected, gives a subscription that is
    /// never told.
    pub fn subscribe<F>(&self, subscriber: F) -> Subscription
    where
        F: Fn(&Lua, Value, Value) + Send + Sync + 'static,
    {
        self.add(Arc::new(subscriber), None)
    }

    /// As [`subscribe`](Self::subscribe), and calls `on_close` once when the cell closes, as
    /// [`Table::subscribe_with_close`](super::Table::subscribe_with_close) does for a table: when
    /// either side closes it, when it is collected, or when the runtime is dropped, given no `Lua`
    /// and with no lock of this module's held. It is not called once the subscription is dropped.
    ///
    /// One run by collection or by dropping the runtime must not call into Lua: a [`Cell`]
    /// refuses to get, set or close from there, with an error. A panic in one run by an explicit
    /// close goes on; in one run by collection or dropping it is written to standard error, and
    /// the rest are still told.
    ///
    /// Subscribing to one that is closed, or has been collected, calls `on_close` at once, before
    /// this returns.
    pub fn subscribe_with_close<F, C>(&self, subscriber: F, on_close: C) -> Subscription
    where
        F: Fn(&Lua, Value, Value) + Send + Sync + 'static,
        C: FnOnce() + Send + 'static,
    {
        self.add(Arc::new(subscriber), Some(Box::new(on_close)))
    }

    /// Closes the cell, as `c:close()` does, telling every subscriber, Lua's included, before it
    /// returns: sets then fail, and gets still work. Closing one that is closed, or has been
    /// collected, does nothing.
    pub fn close(&self) -> Result<()> {
        match self.state.upgrade() {
            Some(state) if !state.is_closed() => self.handle.call(HOST_CLOSE, self.handle.id),
            _ => Ok(()),
        }
    }

    fn add(&self, subscriber: Arc<CellSubscriber>, on_close: Option<OnClose>) -> Subscription {
        shared::add_rust(&self.state, |state| state, subscriber, on_close)
    }
}

impl fmt::Debug for Cell {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Cell")
            .field("id", &self.handle.id)
            .finish_non_exhaustive()
    }
}

impl FromLua for Cell {
    fn from_lua(value: Value, _: &Lua) -> Result<Self> {
        let this = as_bridge_cell(value).map_err(|other| mlua::Error::FromLuaConversionError {
            from: other.type_name(),
            to: "bridge::Cell".to_owned(),
            message: Some(format!("expected a {}", CELL.name)),
        })?;
        let state = state_of(&this)?;
        Ok(Cell {
            handle: Handle::new(&state),
            state: Arc::downgrade(&state),
        })
    }
}
