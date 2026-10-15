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
//! Lua's, in [`TableState`], with the Rust subscribers. The host's handle on a Bridge table,
//! [`Table`], is likewise weak: an id it looks up in a weak-valued table, through a `WeakLua`, and
//! never an `AnyUserData` or an `mlua::Table`, which are registry references too.
//!
//! A subscriber is called inside the write, with no lock held, since it may write in turn or
//! subscribe or unsubscribe, and a Rust subscriber may be running while Lua is mid-call.
//!
//! Nothing here sets a global. [`rust_half`] builds the module's table, and `modules.rs` hands it
//! to `lua/bridge.lua`, which returns it.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};

use mlua::{
    AnyUserData, FromLua, Function, IntoLua, Lua, MetaMethod, MultiValue, Table as LuaTable,
    UserData, Value, WeakLua,
};

type Result<T> = mlua::Result<T>;

/// The name a Bridge table gives itself, as its `__name` and in `tostring`.
const TABLE_NAME: &str = "bridge.table";

/// The name of the handle `bridge.table.subscribe` gives back.
const SUBSCRIPTION_NAME: &str = "bridge.subscription";

/// Where `rust_half` keeps the iterator `__pairs` gives, which walks a Bridge table's backing table
/// with the `next` there was when the module was built, so that a script that later replaces the
/// global `next` does not change how a Bridge table walks.
const PAIRS_ITERATOR: &str = "avarice.bridge.pairs";

/// Where `rust_half` keeps the weak-valued table through which a [`Table`] finds its userdata: by
/// the id in its [`TableState`], so that the host's hold does not keep a Bridge table alive.
const HANDLES: &str = "avarice.bridge.handles";

/// The user value of a Bridge table's userdata that holds its backing table.
const BACKING: usize = 1;

/// The user value of a Bridge table's userdata that holds its Lua subscribers, by subscription id,
/// once it has had one.
const LUA_SUBSCRIBERS: usize = 2;

/// The user value of a subscription handle's userdata that holds the Bridge table it is of.
const SUBSCRIBED_TO: usize = 1;

/// Gives out the ids of Bridge tables and of subscriptions, which are never reused.
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn next_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

/// Builds the module's table: `bridge.table`, a table whose `__call` makes a Bridge table and
/// whose fields are the operations on one.
pub fn rust_half(lua: &Lua) -> Result<Value> {
    let next: Function = lua.globals().raw_get("next")?;
    // `pairs` passes it the userdata as its state, so it is all a script is given.
    let iterator = lua.create_function(move |_, (this, key): (AnyUserData, Value)| {
        next.call::<MultiValue>((backing(&this)?, key))
    })?;
    lua.set_named_registry_value(PAIRS_ITERATOR, iterator)?;

    let handles = lua.create_table()?;
    let weak_values = lua.create_table()?;
    weak_values.set("__mode", "v")?;
    handles.set_metatable(Some(weak_values))?;
    lua.set_named_registry_value(HANDLES, handles)?;

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
    table_fn.set(
        "subscribe",
        lua.create_function(|lua, values: MultiValue| {
            const WHAT: &str = "bridge.table.subscribe";
            let [this, subscriber] = args(values, WHAT)?;
            let this = bridge_table(this, WHAT)?;
            let Value::Function(subscriber) = subscriber else {
                return Err(error(format!(
                    "{WHAT}: expected a function, got {}",
                    subscriber.type_name()
                )));
            };
            subscribe_lua(lua, this, subscriber)
        })?,
    )?;

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
    let state = TableState {
        id: next_id(),
        subscribers: Mutex::new(Vec::new()),
    };
    let this = lua.create_userdata(BridgeTable(Arc::new(state)))?;
    this.set_nth_user_value(BACKING, backing)?;
    Ok(this)
}

/// `value` as a Bridge table's userdata, or an error naming `what` was given something else.
fn bridge_table(value: Value, what: &str) -> Result<AnyUserData> {
    match value {
        Value::UserData(this) if this.is::<BridgeTable>() => Ok(this),
        other => Err(error(format!(
            "{what}: expected a {TABLE_NAME}, got {}",
            other.type_name()
        ))),
    }
}

/// The backing table of `this`, which must be a Bridge table's userdata.
fn backing(this: &AnyUserData) -> Result<LuaTable> {
    if !this.is::<BridgeTable>() {
        return Err(error(format!("expected a {TABLE_NAME}")));
    }
    this.nth_user_value(BACKING)
}

fn state_of(this: &AnyUserData) -> Result<Arc<TableState>> {
    Ok(Arc::clone(&this.borrow::<BridgeTable>()?.0))
}

/// Writes `value` at `key` in the Bridge table `this`, from Lua or from Rust, and tells every
/// subscriber once it is in place.
fn write(lua: &Lua, this: &AnyUserData, key: Value, value: Value) -> Result<()> {
    // As a table would. mlua leaves these to Lua, which in a build with its API checks on asserts
    // rather than raising.
    match key {
        Value::Nil => return Err(error("index is nil".to_owned())),
        Value::Number(n) if n.is_nan() => return Err(error("index is NaN".to_owned())),
        _ => {}
    }
    let backing = backing(this)?;
    let old: Value = backing.raw_get(&key)?;
    backing.raw_set(&key, &value)?;
    let change = Change {
        key,
        old,
        new: value,
    };
    tell(lua, this, std::slice::from_ref(&change))
}

/// Tells every subscriber of `this` of `changes`, in the order they subscribed.
///
/// The list is copied first, and no lock is held while a subscriber runs, since one may write to
/// the table, which nests, or subscribe or unsubscribe. One that subscribes while this runs is not
/// in the copy, so it hears only later changes; one that unsubscribes is looked for again before
/// it is called, so it stops at once.
fn tell(lua: &Lua, this: &AnyUserData, changes: &[Change]) -> Result<()> {
    let state = state_of(this)?;
    let subscribers = state.subscribers().clone();
    if subscribers.is_empty() {
        return Ok(());
    }
    let lua_subscribers: Option<LuaTable> = this.nth_user_value(LUA_SUBSCRIBERS)?;
    for (id, subscriber) in subscribers {
        match subscriber {
            Subscriber::Rust(subscriber) => {
                if state.is_subscribed(id) {
                    subscriber(lua, changes);
                }
            }
            Subscriber::Lua => {
                let Some(lua_subscribers) = &lua_subscribers else {
                    continue;
                };
                let Some(subscriber) = lua_subscribers.raw_get::<Option<Function>>(id)? else {
                    continue;
                };
                let list = lua.create_table_with_capacity(changes.len(), 0)?;
                for change in changes {
                    let entry = lua.create_table_with_capacity(0, 3)?;
                    entry.raw_set("key", &change.key)?;
                    entry.raw_set("old", &change.old)?;
                    entry.raw_set("new", &change.new)?;
                    list.raw_push(entry)?;
                }
                // ADR 0022: the write has succeeded whatever a subscriber does, and the others are
                // still told. Reported as Astra reports a task's error, but on standard error.
                if let Err(e) = subscriber.call::<()>(list) {
                    eprintln!("Error running a {TABLE_NAME} subscriber: {e}");
                }
            }
        }
    }
    Ok(())
}

/// `bridge.table.subscribe(t, fn)`: adds `subscriber`, kept in a user value of `this` so that the
/// collector traces it, and gives back a handle whose `unsubscribe` removes it.
fn subscribe_lua(lua: &Lua, this: AnyUserData, subscriber: Function) -> Result<AnyUserData> {
    let state = state_of(&this)?;
    let lua_subscribers = match this.nth_user_value::<Option<LuaTable>>(LUA_SUBSCRIBERS)? {
        Some(table) => table,
        None => {
            let table = lua.create_table()?;
            this.set_nth_user_value(LUA_SUBSCRIBERS, &table)?;
            table
        }
    };
    let id = next_id();
    lua_subscribers.raw_set(id, subscriber)?;
    state.subscribers().push((id, Subscriber::Lua));
    let handle = lua.create_userdata(LuaSubscription { state, id })?;
    handle.set_nth_user_value(SUBSCRIBED_TO, this)?;
    Ok(handle)
}

/// A Rust subscriber to a Bridge table.
type RustSubscriber = dyn Fn(&Lua, &[Change]) + Send + Sync;

/// One of a Bridge table's subscribers. A Lua one's function is not here but in the userdata's
/// user value, under the same id, for the reasons the module's header gives.
#[derive(Clone)]
enum Subscriber {
    Rust(Arc<RustSubscriber>),
    Lua,
}

/// The part of a Bridge table that is not Lua's, shared between its userdata and the host's
/// handles. It holds no Lua value, for the reasons the module's header gives: the backing table
/// and the Lua subscribers' functions are user values of the userdata. It will hold whether the
/// table is closed, and the batch depth.
struct TableState {
    /// The key the host's handles find the userdata by, in the table at [`HANDLES`].
    id: u64,
    /// Every subscriber, Lua's and Rust's, by subscription id, in the order they subscribed.
    subscribers: Mutex<Vec<(u64, Subscriber)>>,
}

impl TableState {
    /// The subscribers, locked. Never held across a call to one.
    fn subscribers(&self) -> MutexGuard<'_, Vec<(u64, Subscriber)>> {
        // A panic while it is held leaves it as it was, since nothing runs then but a push or a
        // retain.
        self.subscribers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn is_subscribed(&self, id: u64) -> bool {
        self.subscribers().iter().any(|(each, _)| *each == id)
    }

    fn unsubscribe(&self, id: u64) {
        self.subscribers().retain(|(each, _)| *each != id);
    }
}

/// The userdata Lua holds a Bridge table as. Its first user value is the backing table, and its
/// second, once it has had a Lua subscriber, the Lua subscribers.
struct BridgeTable(Arc<TableState>);

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
            |lua, (this, key, value): (AnyUserData, Value, Value)| write(lua, &this, key, value),
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

/// The handle `bridge.table.subscribe` gives back. Its user value is the Bridge table, whose user
/// value holds the subscriber's function.
struct LuaSubscription {
    state: Arc<TableState>,
    id: u64,
}

impl UserData for LuaSubscription {
    fn add_fields<F: mlua::UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field(MetaMethod::Type, SUBSCRIPTION_NAME);
    }

    fn add_methods<M: mlua::UserDataMethods<Self>>(methods: &mut M) {
        // A second call does nothing.
        methods.add_function("unsubscribe", |_, handle: AnyUserData| {
            let (state, id) = {
                let this = handle.borrow::<LuaSubscription>()?;
                (Arc::clone(&this.state), this.id)
            };
            state.unsubscribe(id);
            let table: AnyUserData = handle.nth_user_value(SUBSCRIBED_TO)?;
            if let Some(lua_subscribers) =
                table.nth_user_value::<Option<LuaTable>>(LUA_SUBSCRIBERS)?
            {
                lua_subscribers.raw_set(id, Value::Nil)?;
            }
            Ok(())
        });
    }
}

/// One change to a Bridge table, as a subscriber is told it: the key written, the value it had,
/// and the value it has now. Nil stands for a key that was not there, or is no longer.
#[derive(Clone, Debug)]
pub struct Change {
    pub key: Value,
    pub old: Value,
    pub new: Value,
}

/// The host's handle on a Bridge table, received from Lua through [`FromLua`].
///
/// It holds the Bridge table weakly: Lua owns it, and collects it once no Lua value refers to it,
/// whether or not the host still has a handle (ADR 0022). Reading or writing through a handle on
/// one that has been collected, or whose `Lua` is gone, returns an error.
///
/// Even its hold on the part of the table that is not Lua's is weak, so that a subscriber may
/// keep a handle on its own table without the two keeping each other alive.
#[derive(Clone)]
pub struct Table {
    state: Weak<TableState>,
    id: u64,
    lua: WeakLua,
}

impl Table {
    /// The value at `key`, as a script's `t[key]` reads it.
    pub fn get<V: FromLua>(&self, key: impl IntoLua) -> Result<V> {
        let (lua, this) = self.userdata()?;
        V::from_lua(backing(&this)?.raw_get(key)?, &lua)
    }

    /// Writes `value` at `key`, as a script's `t[key] = value` does, and tells every subscriber,
    /// Lua's included, before it returns.
    pub fn set(&self, key: impl IntoLua, value: impl IntoLua) -> Result<()> {
        let (lua, this) = self.userdata()?;
        let key = key.into_lua(&lua)?;
        let value = value.into_lua(&lua)?;
        write(&lua, &this, key, value)
    }

    /// Calls `subscriber` with every change to the table from now on, until the returned
    /// [`Subscription`] is dropped.
    ///
    /// It runs on the Lua thread, inside the write that made the change, once the change is in
    /// place, and the script that wrote waits for it: one with slow work to do hands it off and
    /// returns. It may write to the table, which tells every subscriber of that write before it
    /// returns. An `mlua::Value` of the table that it keeps keeps the table alive.
    ///
    /// Subscribing to one that has been collected gives a subscription that is never told.
    pub fn subscribe<F>(&self, subscriber: F) -> Subscription
    where
        F: Fn(&Lua, &[Change]) + Send + Sync + 'static,
    {
        let id = next_id();
        if let Some(state) = self.state.upgrade() {
            state
                .subscribers()
                .push((id, Subscriber::Rust(Arc::new(subscriber))));
        }
        Subscription {
            state: Weak::clone(&self.state),
            id,
        }
    }

    /// The `Lua` and the Bridge table's userdata, if both are still there.
    fn userdata(&self) -> Result<(Lua, AnyUserData)> {
        let gone = || error(format!("{TABLE_NAME}: the Bridge table has been collected"));
        let lua = self.lua.try_upgrade().ok_or_else(gone)?;
        let handles: LuaTable = lua.named_registry_value(HANDLES)?;
        let this = handles
            .raw_get::<Option<AnyUserData>>(self.id)?
            .ok_or_else(gone)?;
        Ok((lua, this))
    }
}

impl FromLua for Table {
    fn from_lua(value: Value, lua: &Lua) -> Result<Self> {
        let this = match value {
            Value::UserData(this) if this.is::<BridgeTable>() => this,
            other => {
                return Err(mlua::Error::FromLuaConversionError {
                    from: other.type_name(),
                    to: "bridge::Table".to_owned(),
                    message: Some(format!("expected a {TABLE_NAME}")),
                });
            }
        };
        let state = state_of(&this)?;
        let handles: LuaTable = lua.named_registry_value(HANDLES)?;
        handles.raw_set(state.id, &this)?;
        Ok(Table {
            state: Arc::downgrade(&state),
            id: state.id,
            lua: lua.weak(),
        })
    }
}

/// A Rust subscription to a Bridge table, from [`Table::subscribe`]. Dropping it unsubscribes, at
/// once: a subscriber is not called again once its subscription is dropped, even by a write it is
/// being told of.
#[must_use = "dropping a Subscription unsubscribes"]
pub struct Subscription {
    state: Weak<TableState>,
    id: u64,
}

impl Drop for Subscription {
    fn drop(&mut self) {
        if let Some(state) = self.state.upgrade() {
            state.unsubscribe(self.id);
        }
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
