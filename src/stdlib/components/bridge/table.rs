//! A **Bridge table**: a value a script writes and reads as a Lua table, which the host can
//! watch.
//!
//! A Bridge table is a userdata whose contents are an ordinary Lua table, its backing table. Its
//! metamethods reach the backing table and nothing else, and it has no methods, so every key is
//! the script's: whatever is done to one besides reading and writing it is a function of
//! `bridge.table`, taking it first. `__pairs` and `__len` make `pairs`, `ipairs`, `#` and the
//! `table` library work on it as on a table, and the backing table itself never reaches a script,
//! which could otherwise write past the Bridge table: `snapshot` and `__view`, which `print` shows
//! (ADR 0023), give a copy of it, and `serde` encodes it through [`shared::serialize`].
//!
//! The backing table is the userdata's user value [`BACKING`]; the part that is not Lua's is in
//! [`TableState`].
//!
//! A batch holds back the changes made inside it, and the outermost tells them as one list when
//! it ends. They hold Lua values, so they are a user value too, for as long as the batch is open,
//! which is as long as the call to `bridge.table.batch` is on the stack; the depth, which is not
//! Lua's, is in [`TableState`]. The list is taken before it is told, so a subscriber that writes
//! is told of its own write on its own, as outside a batch.

use std::fmt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Weak};

use mlua::{
    AnyUserData, FromLua, Function, IntoLua, IntoLuaMulti, Lua, MetaMethod, MultiValue,
    Table as LuaTable, UserData, Value,
};

use serde::{Serialize, Serializer};

use super::shared::{self, Core, Handle, Kind, OnClose, Subscribed, Subscription};
use super::{Depth, Helpers, Result, args, error, is_limit};
use crate::error::was_out_of_memory;

/// What a Bridge table is called, as its `__name` and in `tostring`, and in prose.
static TABLE: Kind = Kind {
    name: "bridge.table",
    noun: "Bridge table",
};

/// Where [`build`] keeps the iterator `__pairs` gives, which walks a Bridge table's backing table
/// with the `next` there was when the module was built, so that a script that later replaces the
/// global `next` does not change how a Bridge table walks.
const PAIRS_ITERATOR: &str = "avarice.bridge.pairs";

/// Where [`build`] keeps the functions through which a [`Table`] reads, writes and closes. Each is
/// called into Lua, so that the whole of a read, or of a write and the telling of it, runs under
/// the Lua lock, as one call, even when the host makes it from another thread.
const HOST_READ: &str = "avarice.bridge.read";
const HOST_WRITE: &str = "avarice.bridge.write";
const HOST_CLOSE: &str = "avarice.bridge.close";

/// Where [`build`] keeps the Lua function that copies a batch's list for a Lua subscriber.
const COPY_CHANGES: &str = "avarice.bridge.copy";

// User values, by index, besides the shared ones, `LUA_SUBSCRIBERS` and `LUA_ON_CLOSE`.

/// The user value of a Bridge table's userdata that holds its backing table.
const BACKING: usize = 1;

/// The user value of a Bridge table's userdata that holds, while a batch is open, the changes it
/// will tell when it ends: a list of `{ key, old, new }` tables, in write order.
const PENDING: usize = 4;

/// The copying of a batch's list for a Lua subscriber, which stays in Lua so that a long list
/// never takes a Rust reference per value.
const COPY: &str = "
    return function(pending)
        local list = {}
        for i = 1, #pending do
            local change = pending[i]
            list[i] = { key = change.key, old = change.old, new = change.new }
        end
        return list
    end
";

/// Builds `bridge.table`, a table whose `__call` makes a Bridge table and whose fields are the
/// operations on one.
pub(super) fn build(lua: &Lua, helpers: &Helpers) -> Result<LuaTable> {
    let next: Function = lua.globals().raw_get("next")?;
    // `pairs` passes it the userdata as its state, so it is all a script is given.
    let iterator = lua.create_function(move |_, (this, key): (Value, Value)| {
        next.call::<MultiValue>((backing(&bridge_table(this, TABLE.name)?)?, key))
    })?;
    lua.set_named_registry_value(PAIRS_ITERATOR, iterator)?;

    let read = lua.create_function(|lua, (id, key): (u64, Value)| {
        backing(&shared::handled(lua, id, &TABLE)?)?.raw_get::<Value>(key)
    })?;
    lua.set_named_registry_value(HOST_READ, read)?;
    let write = lua.create_function(|lua, (id, key, value): (u64, Value, Value)| {
        self::write(lua, &shared::handled(lua, id, &TABLE)?, key, value)
    })?;
    lua.set_named_registry_value(HOST_WRITE, write)?;
    let host_close =
        lua.create_function(|lua, id: u64| close(lua, &shared::handled(lua, id, &TABLE)?))?;
    lua.set_named_registry_value(HOST_CLOSE, host_close)?;
    let copy: Function = lua.load(COPY).set_name("=[bridge]").call(())?;
    lua.set_named_registry_value(COPY_CHANGES, copy)?;

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
            let [this, subscriber, on_close] = args(values, WHAT)?;
            let this = bridge_table(this, WHAT)?;
            let subscriber = shared::subscriber_args(subscriber, on_close, WHAT)?;
            let state = state_of(&this)?;
            shared::subscribe_lua(lua, this, state, |state| &state.core, subscriber, WHAT)
        })?,
    )?;
    table_fn.set(
        "snapshot",
        lua.create_function(|lua, values: MultiValue| {
            let [this] = args(values, "bridge.table.snapshot")?;
            snapshot(lua, &bridge_table(this, "bridge.table.snapshot")?)
        })?,
    )?;
    table_fn.set(
        "close",
        lua.create_function(|lua, values: MultiValue| {
            let [this] = args(values, "bridge.table.close")?;
            close(lua, &bridge_table(this, "bridge.table.close")?)
        })?,
    )?;

    let protecting = helpers.clone();
    let run = lua.create_function(move |lua, values: MultiValue| {
        const WHAT: &str = "bridge.table.batch";
        let [this, body] = args(values, WHAT)?;
        let this = open_table(this, WHAT)?;
        let Value::Function(body) = body else {
            return Err(error(format!(
                "{WHAT}: expected a function, got {}",
                body.type_name()
            )));
        };
        batched(lua, &this, || {
            match protecting.protect(lua, WHAT, body, ())? {
                Ok(_) => Ok((true, Value::Nil)),
                Err(raised) => Ok((false, raised)),
            }
        })
    })?;
    table_fn.set("batch", helpers.raising.call::<Function>(run)?)?;

    table_fn.set(
        "insert",
        lua.create_function(|lua, values: MultiValue| {
            const WHAT: &str = "bridge.table.insert";
            let given = values.len();
            if !(2..=3).contains(&given) {
                return Err(error(format!("{WHAT}: wrong number of arguments")));
            }
            let [this, pos, value] = args(values, WHAT)?;
            let this = open_table(this, WHAT)?;
            let (pos, value) = match given {
                2 => (None, pos),
                _ => (Some(position(lua, pos, WHAT)?), value),
            };
            insert(lua, &this, pos, value)
        })?,
    )?;
    table_fn.set(
        "remove",
        lua.create_function(|lua, values: MultiValue| {
            const WHAT: &str = "bridge.table.remove";
            let [this, pos] = args(values, WHAT)?;
            let this = open_table(this, WHAT)?;
            let pos = match pos {
                Value::Nil => None,
                pos => Some(position(lua, pos, WHAT)?),
            };
            remove(lua, &this, pos)
        })?,
    )?;
    Ok(table_fn)
}

/// Runs `body` inside a batch of `this`: every write to `this` until it returns, by any path, is
/// held back, and the outermost batch tells them all as one list, in write order, when it ends,
/// whether or not `body` failed. The list is taken before it is told, so a subscriber that writes
/// is told of its write on its own, as a write outside a batch is. A limit error from `body` is
/// what goes on, whatever the telling does, and after a memory error nothing is told.
fn batched<R>(lua: &Lua, this: &AnyUserData, body: impl FnOnce() -> Result<R>) -> Result<R> {
    let state = state_of(this)?;
    let (outermost, result) = {
        let depth = Depth::enter(&state.batch_depth);
        if depth.outermost {
            // Any left by a batch that a panic unwound out of was never told, and is not now.
            this.set_nth_user_value(PENDING, Value::Nil)?;
        }
        (depth.outermost, body())
    };
    if !outermost {
        return result;
    }
    let pending = this.nth_user_value::<Option<LuaTable>>(PENDING);
    let cleared = this.set_nth_user_value(PENDING, Value::Nil);
    match result {
        // Nothing is told once memory has run out: there is none to tell with, and mlua panics
        // rather than failing when it cannot make room for a Rust subscriber's values.
        Err(e) if was_out_of_memory(&e) => Err(e),
        result => {
            let told = match (pending, cleared) {
                (Ok(Some(pending)), Ok(())) => tell(lua, this, Changes::Batch(&pending)),
                (Ok(None), Ok(())) => Ok(()),
                (Err(e), _) | (_, Err(e)) => Err(e),
            };
            match result {
                Err(e) if is_limit(&e) => Err(e),
                result => told.and(result),
            }
        }
    }
}

/// `bridge.table.insert(t, pos?, value)`: as `table.insert`, `value` at `pos`, or at the end, the
/// items from `pos` on moved up one, all as one batch.
fn insert(lua: &Lua, this: &AnyUserData, pos: Option<i64>, value: Value) -> Result<()> {
    let backing = backing(this)?;
    let end = backing.raw_len() as i64 + 1;
    let pos = pos.unwrap_or(end);
    if !(1..=end).contains(&pos) {
        return Err(error(
            "bridge.table.insert: position out of bounds".to_owned(),
        ));
    }
    batched(lua, this, || {
        for i in (pos + 1..=end).rev() {
            write(lua, this, Value::Integer(i), backing.raw_get(i - 1)?)?;
        }
        write(lua, this, Value::Integer(pos), value)
    })
}

/// `bridge.table.remove(t, pos?)`: as `table.remove`, the item at `pos`, or the last, taken out and
/// given back, the items after it moved down one, all as one batch.
fn remove(lua: &Lua, this: &AnyUserData, pos: Option<i64>) -> Result<Value> {
    let backing = backing(this)?;
    let size = backing.raw_len() as i64;
    let pos = pos.unwrap_or(size);
    // As `table.remove` has it: the length itself, even 0, or 1 to one past the length.
    if pos != size && !(1..=size + 1).contains(&pos) {
        return Err(error(
            "bridge.table.remove: position out of bounds".to_owned(),
        ));
    }
    let removed: Value = backing.raw_get(pos)?;
    batched(lua, this, || {
        for i in pos..size {
            write(lua, this, Value::Integer(i), backing.raw_get(i + 1)?)?;
        }
        write(lua, this, Value::Integer(pos.max(size)), Value::Nil)
    })?;
    Ok(removed)
}

/// `pos` as an integer, as `table.insert` and `table.remove` take one, or an error naming `what`.
fn position(lua: &Lua, pos: Value, what: &str) -> Result<i64> {
    let type_name = pos.type_name();
    lua.coerce_integer(pos)?.ok_or_else(|| {
        error(format!(
            "{what}: expected an integer position, got {type_name}"
        ))
    })
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
        core: Core::new(lua, &TABLE),
        batch_depth: AtomicUsize::new(0),
    };
    let this = lua.create_ser_userdata(BridgeTable(Arc::new(state)))?;
    shared::enter(lua, &this, &this.borrow::<BridgeTable>()?.0.core)?;
    this.set_nth_user_value(BACKING, backing)?;
    Ok(this)
}

/// `bridge.table.snapshot(t)`: a plain table holding what the Bridge table `this` holds, copied
/// shallowly, so that writing to it writes past no subscriber.
fn snapshot(lua: &Lua, this: &AnyUserData) -> Result<LuaTable> {
    let backing = backing(this)?;
    let copy = lua.create_table_with_capacity(backing.raw_len(), 0)?;
    backing.for_each(|k: Value, v: Value| copy.raw_set(k, v))?;
    Ok(copy)
}

/// `value` as a Bridge table's userdata, or `value` back if it is not one. Everything that takes
/// a Bridge table from Lua comes through here.
fn as_bridge_table(value: Value) -> std::result::Result<AnyUserData, Value> {
    match value {
        Value::UserData(this) if this.is::<BridgeTable>() => Ok(this),
        other => Err(other),
    }
}

/// `value` as a Bridge table's userdata, or an error naming `what` was given something else.
fn bridge_table(value: Value, what: &str) -> Result<AnyUserData> {
    as_bridge_table(value).map_err(|other| {
        error(format!(
            "{what}: expected a {}, got {}",
            TABLE.name,
            other.type_name()
        ))
    })
}

/// `value` as a Bridge table's userdata that is not closed, or an error naming `what`.
fn open_table(value: Value, what: &str) -> Result<AnyUserData> {
    let this = bridge_table(value, what)?;
    if state_of(&this)?.core.is_closed() {
        return Err(shared::is_closed(&TABLE, what));
    }
    Ok(this)
}

/// The backing table of `this`, a Bridge table's userdata from [`bridge_table`].
fn backing(this: &AnyUserData) -> Result<LuaTable> {
    this.nth_user_value(BACKING)
}

fn state_of(this: &AnyUserData) -> Result<Arc<TableState>> {
    Ok(Arc::clone(&this.borrow::<BridgeTable>()?.0))
}

/// Writes `value` at `key` in the Bridge table `this`, from Lua or from Rust, and tells every
/// subscriber once it is in place, or, while a batch of it is open, adds the change to the batch.
fn write(lua: &Lua, this: &AnyUserData, key: Value, value: Value) -> Result<()> {
    // As a table would. mlua leaves these to Lua, which in a build with its API checks on asserts
    // rather than raising.
    match key {
        Value::Nil => return Err(error("index is nil".to_owned())),
        Value::Number(n) if n.is_nan() => return Err(error("index is NaN".to_owned())),
        _ => {}
    }
    let state = state_of(this)?;
    if state.core.is_closed() {
        return Err(shared::is_closed(&TABLE, TABLE.name));
    }
    let backing = backing(this)?;
    let old: Value = backing.raw_get(&key)?;
    backing.raw_set(&key, &value)?;
    if state.batch_depth.load(Ordering::Relaxed) > 0 {
        let pending = shared::user_table(lua, this, PENDING)?;
        return pending.raw_push(change_entry(lua, &key, &old, &value)?);
    }
    let change = Change {
        key,
        old,
        new: value,
    };
    tell(lua, this, Changes::One(&change))
}

/// One change as Lua is given it, and as a batch holds it back: `{ key = k, old = o, new = n }`.
fn change_entry(lua: &Lua, key: &Value, old: &Value, new: &Value) -> Result<LuaTable> {
    let entry = lua.create_table_with_capacity(0, 3)?;
    entry.raw_set("key", key)?;
    entry.raw_set("old", old)?;
    entry.raw_set("new", new)?;
    Ok(entry)
}

/// What [`tell`] tells: one write's change, or a batch's list of them, as Lua holds it.
#[derive(Clone, Copy)]
enum Changes<'a> {
    One(&'a Change),
    Batch(&'a LuaTable),
}

/// Tells every subscriber of `this` of `changes`, as [`shared::tell`] does.
fn tell(lua: &Lua, this: &AnyUserData, changes: Changes<'_>) -> Result<()> {
    tell_to(lua, this, changes, None)
}

/// [`tell`], or, when `taken` is given, [`shared::tell_taken`] to the Lua subscribers [`close`]
/// took out.
///
/// A batch's list is made into [`Change`]s only for a Rust subscriber, since each value of one
/// holds a slot of mlua's reference stack, and then a slice at a time, of at most [`RUST_SLICE`]:
/// a Lua subscriber's list is copied in Lua, whole.
fn tell_to(
    lua: &Lua,
    this: &AnyUserData,
    changes: Changes<'_>,
    taken: Option<Option<LuaTable>>,
) -> Result<()> {
    let state = state_of(this)?;
    let core = &state.core;
    let rust = |id: u64, subscriber: &TableSubscriber| -> Result<()> {
        match changes {
            Changes::One(change) => subscriber(lua, std::slice::from_ref(change)),
            Changes::Batch(pending) => {
                let len = pending.raw_len();
                let mut from = 1;
                while from <= len {
                    let to = len.min(from + RUST_SLICE - 1);
                    let slice = rust_changes(pending, from..=to)?;
                    subscriber(lua, &slice);
                    drop(slice);
                    from = to + 1;
                    if from <= len && !core.is_subscribed(id) {
                        break;
                    }
                }
            }
        }
        Ok(())
    };
    let lua_args = || -> Result<MultiValue> {
        let list = match changes {
            Changes::One(change) => {
                let list = lua.create_table_with_capacity(1, 0)?;
                list.raw_push(change_entry(lua, &change.key, &change.old, &change.new)?)?;
                list
            }
            Changes::Batch(pending) => lua
                .named_registry_value::<Function>(COPY_CHANGES)?
                .call(pending)?,
        };
        list.into_lua_multi(lua)
    };
    match taken {
        None => shared::tell(this, core, rust, lua_args),
        Some(lua_subscribers) => shared::tell_taken(this, core, lua_subscribers, rust, lua_args),
    }
}

/// The most changes a Rust subscriber is given in one call. A batch's list longer than this
/// reaches it as several calls, in order, each slice's values dropped before the next is made, so
/// that mlua never holds more than a slice of them: it has room for about a million, and panics
/// rather than failing past that.
const RUST_SLICE: usize = 10_000;

/// The entries `range` of a batch's list, as Lua holds it, as [`Change`]s for a Rust subscriber.
fn rust_changes(pending: &LuaTable, range: std::ops::RangeInclusive<usize>) -> Result<Vec<Change>> {
    range
        .map(|i| {
            let entry: LuaTable = pending.raw_get(i)?;
            Ok(Change {
                key: entry.raw_get("key")?,
                old: entry.raw_get("old")?,
                new: entry.raw_get("new")?,
            })
        })
        .collect()
}

/// `bridge.table.close(t)`, and [`Table::close`]: closes `this`, as [`shared::close`] does.
///
/// Inside a batch, the changes the batch holds back are told first, so that every write made before
/// the close is told before it; the batch then has nothing left to tell when it ends, and any write
/// after the close raises.
fn close(lua: &Lua, this: &AnyUserData) -> Result<()> {
    let state = state_of(this)?;
    shared::close(this, &state.core, |lua_subscribers| {
        let pending = match state.batch_depth.load(Ordering::Relaxed) > 0 {
            true => shared::take_user_value(this, PENDING)?,
            false => None,
        };
        match pending {
            Some(pending) => tell_to(lua, this, Changes::Batch(&pending), Some(lua_subscribers)),
            None => Ok(()),
        }
    })
}

/// A Rust subscriber to a Bridge table.
type TableSubscriber = dyn Fn(&Lua, &[Change]) + Send + Sync;

/// The part of a Bridge table that is not Lua's, shared between its userdata and the host's
/// handles. It holds no Lua value, for the reasons the module's header gives: the backing table
/// and the Lua subscribers' functions are user values of the userdata, and so are the changes a
/// batch holds back, and the Lua subscribers' close callbacks.
struct TableState {
    core: Core<TableSubscriber>,
    /// How many batches of this table are open, one inside another. Only the Lua thread, under the
    /// Lua lock, reads or writes it.
    batch_depth: AtomicUsize,
}

impl Subscribed for TableState {
    fn unsubscribe(&self, id: u64) {
        self.core.unsubscribe(id);
    }
}

/// The userdata Lua holds a Bridge table as. Its first user value is the backing table, its
/// second, once it has had a Lua subscriber, the Lua subscribers, its third, once a Lua subscriber
/// has given one, the Lua close callbacks, and its fourth, while a batch is open and has been
/// written to, the changes the batch holds back.
struct BridgeTable(Arc<TableState>);

impl UserData for BridgeTable {
    fn add_fields<F: mlua::UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field(MetaMethod::Type, TABLE.name);
    }

    fn add_methods<M: mlua::UserDataMethods<Self>>(methods: &mut M) {
        // A script can take these out of the metatable and call them with something else, so each
        // checks what it was given.
        methods.add_meta_function(MetaMethod::Index, |_, (this, key): (Value, Value)| {
            backing(&bridge_table(this, TABLE.name)?)?.raw_get::<Value>(key)
        });
        methods.add_meta_function(
            MetaMethod::NewIndex,
            |lua, (this, key, value): (Value, Value, Value)| {
                write(lua, &bridge_table(this, TABLE.name)?, key, value)
            },
        );
        methods.add_meta_function(MetaMethod::Len, |_, this: Value| {
            Ok(backing(&bridge_table(this, TABLE.name)?)?.raw_len())
        });
        methods.add_meta_function(MetaMethod::Pairs, |lua, this: Value| {
            let iterator: Function = lua.named_registry_value(PAIRS_ITERATOR)?;
            Ok((iterator, bridge_table(this, TABLE.name)?, Value::Nil))
        });
        // What `print` shows (ADR 0023): a copy, not the backing table, which whatever holds the
        // backing table could write past the subscribers. `print` would not, but anything that
        // can reach the metatable can call this, and the copy costs no more than `print`'s walk.
        methods.add_meta_function("__view", |lua, this: Value| {
            snapshot(lua, &bridge_table(this, TABLE.name)?)
        });
        methods.add_meta_function(MetaMethod::ToString, |_, this: AnyUserData| {
            Ok(format!(
                "{}: {:p}",
                TABLE.name,
                Value::UserData(this).to_pointer()
            ))
        });
    }
}

/// `serde` encodes a Bridge table as its backing table.
impl Serialize for BridgeTable {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        shared::serialize(&self.0.core, serializer, |this| {
            backing(this).map(Value::Table)
        })
    }
}

/// Collection closes a Bridge table nobody closed, and so does dropping the runtime, as
/// [`Core::close_by_collection`] has it.
impl Drop for BridgeTable {
    fn drop(&mut self) {
        self.0.core.close_by_collection();
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
/// whether or not the host still has a handle (ADR 0022), and so closes it. Writing through a
/// handle on one that is closed returns an error, and so does reading or writing through one on
/// one that has been collected, or whose `Lua` is gone.
///
/// Even its hold on the part of the table that is not Lua's is weak, so that a subscriber may
/// keep a handle on its own table without the two keeping each other alive.
#[derive(Clone)]
pub struct Table {
    state: Weak<TableState>,
    handle: Handle,
}

impl Table {
    /// The value at `key`, as a script's `t[key]` reads it.
    pub fn get<V: FromLua>(&self, key: impl IntoLua) -> Result<V> {
        self.handle.call(HOST_READ, (self.handle.id, key))
    }

    /// Writes `value` at `key`, as a script's `t[key] = value` does, and tells every subscriber,
    /// Lua's included, before it returns. The write and the telling are one call into Lua, so they
    /// hold the Lua lock throughout, from whichever thread this is called.
    ///
    /// Writing to one that is closed returns an error, without calling into Lua, so a close
    /// callback may try it even while the table is being collected.
    pub fn set(&self, key: impl IntoLua, value: impl IntoLua) -> Result<()> {
        match self.state.upgrade() {
            Some(state) if state.core.is_closed() => Err(shared::is_closed(&TABLE, TABLE.name)),
            _ => self.handle.call(HOST_WRITE, (self.handle.id, key, value)),
        }
    }

    /// Calls `subscriber` with every change to the table from now on, until the returned
    /// [`Subscription`] is dropped.
    ///
    /// It runs on the Lua thread, inside the write that made the change, once the change is in
    /// place, and the script that wrote waits for it: one with slow work to do hands it off and
    /// returns. It may write to the table, which tells every subscriber of that write before it
    /// returns. An `mlua::Value` of the table that it keeps keeps the table alive.
    ///
    /// A batch of more than 10,000 changes reaches it as several calls, in order, each of at most
    /// 10,000, so that mlua never holds the values of more than that many at once.
    ///
    /// Subscribing to one that is closed, or has been collected, gives a subscription that is
    /// never told.
    pub fn subscribe<F>(&self, subscriber: F) -> Subscription
    where
        F: Fn(&Lua, &[Change]) + Send + Sync + 'static,
    {
        self.add(Arc::new(subscriber), None)
    }

    /// As [`subscribe`](Self::subscribe), and calls `on_close` once when the table closes: when
    /// either side closes it, when it is collected, or when the runtime is dropped. It is given no
    /// `Lua`, since in the last two there is none to give, and it runs with no lock of this
    /// module's held, so it may drop a [`Subscription`]. It is not called once the subscription
    /// is dropped.
    ///
    /// One run by an explicit close may call into Lua, through a [`Table`]. One run by collection
    /// or by dropping the runtime is inside Lua's collector, where no time limit or cancel applies,
    /// and must not: a [`Table`] refuses to read, write or close from there, with an error. A panic
    /// in one run by an explicit close goes on, as a subscriber's does; in one run by collection
    /// or dropping it is written to standard error, and the rest are still told.
    ///
    /// Subscribing to one that is closed, or has been collected, calls `on_close` at once, before
    /// this returns.
    pub fn subscribe_with_close<F, C>(&self, subscriber: F, on_close: C) -> Subscription
    where
        F: Fn(&Lua, &[Change]) + Send + Sync + 'static,
        C: FnOnce() + Send + 'static,
    {
        self.add(Arc::new(subscriber), Some(Box::new(on_close)))
    }

    /// Closes the table, as `bridge.table.close` does, telling every subscriber, Lua's included,
    /// before it returns: writes then fail, and reads still work. Closing one that is closed, or
    /// has been collected, does nothing.
    pub fn close(&self) -> Result<()> {
        match self.state.upgrade() {
            Some(state) if !state.core.is_closed() => self.handle.call(HOST_CLOSE, self.handle.id),
            _ => Ok(()),
        }
    }

    fn add(&self, subscriber: Arc<TableSubscriber>, on_close: Option<OnClose>) -> Subscription {
        shared::add_rust(&self.state, |state| &state.core, subscriber, on_close)
    }
}

impl fmt::Debug for Table {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Table")
            .field("id", &self.handle.id)
            .finish_non_exhaustive()
    }
}

impl FromLua for Table {
    fn from_lua(value: Value, _: &Lua) -> Result<Self> {
        let this = as_bridge_table(value).map_err(|other| mlua::Error::FromLuaConversionError {
            from: other.type_name(),
            to: "bridge::Table".to_owned(),
            message: Some(format!("expected a {}", TABLE.name)),
        })?;
        let state = state_of(&this)?;
        Ok(Table {
            handle: Handle::new(&state.core),
            state: Arc::downgrade(&state),
        })
    }
}
