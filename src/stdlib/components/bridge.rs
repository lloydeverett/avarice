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
//! A subscriber is called inside the write, under the Lua lock, even when the host writes from
//! another thread, and with no lock of this module's held, since it may write in turn or subscribe
//! or unsubscribe. Nor is a subscriber dropped under one, since it may own a subscription whose
//! drop takes it.
//!
//! A Bridge table ends by being closed: by `bridge.table.close`, by [`Table::close`], or by the
//! userdata's `Drop`, when it is collected or the runtime is dropped. Each subscriber is taken out
//! in turn and told by its close callback, outside any lock, so a close callback may drop a
//! subscription. `Drop` has no `Lua`, and Lua may be tearing down, so it tells only the Rust close
//! callbacks, which take none; a Lua one is collected with the table. The flag that it is closed
//! is in [`TableState`], so that the host's handles find it without calling into Lua.
//!
//! A batch holds back the changes made inside it, and the outermost tells them as one list when
//! it ends. They hold Lua values, so they are a user value too, for as long as the batch is open,
//! which is as long as the call to `bridge.table.batch` is on the stack; the depth, which is not
//! Lua's, is in [`TableState`]. The list is taken before it is told, so a subscriber that writes
//! is told of its own write on its own, as outside a batch.
//!
//! Nothing here sets a global. [`rust_half`] builds the module's table, and `modules.rs` hands it
//! to `lua/bridge.lua`, which returns it.

use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};

use mlua::{
    AnyUserData, FromLua, FromLuaMulti, Function, IntoLua, IntoLuaMulti, Lua, MetaMethod,
    MultiValue, Table as LuaTable, UserData, Value, WeakLua,
};

use crate::error::{was_cancelled, was_out_of_memory, was_timed_out};
use crate::lock::lock;

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

/// Where `rust_half` keeps the functions through which a [`Table`] reads and writes. Each is called
/// into Lua, so that the whole of a read, or of a write and the telling of it, runs under the Lua
/// lock, as one call, even when the host makes it from another thread.
const HOST_READ: &str = "avarice.bridge.read";
const HOST_WRITE: &str = "avarice.bridge.write";
const HOST_CLOSE: &str = "avarice.bridge.close";

/// Where `rust_half` keeps the Lua function that copies a batch's list for a Lua subscriber.
const COPY_CHANGES: &str = "avarice.bridge.copy";

// User values, by index. Each userdata numbers its own from 1: `BACKING`, `LUA_SUBSCRIBERS`,
// `PENDING` and `LUA_ON_CLOSE` are a Bridge table's, and `SUBSCRIBED_TO` a subscription handle's.

/// The user value of a Bridge table's userdata that holds its backing table.
const BACKING: usize = 1;

/// The user value of a Bridge table's userdata that holds its Lua subscribers, by subscription id,
/// once it has had one.
const LUA_SUBSCRIBERS: usize = 2;

/// The user value of a Bridge table's userdata that holds, while a batch is open, the changes it
/// will tell when it ends: a list of `{ key, old, new }` tables, in write order.
const PENDING: usize = 3;

/// The user value of a Bridge table's userdata that holds its Lua subscribers' close callbacks, by
/// subscription id, once one has been given.
const LUA_ON_CLOSE: usize = 4;

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
    let iterator = lua.create_function(move |_, (this, key): (Value, Value)| {
        next.call::<MultiValue>((backing(&bridge_table(this, TABLE_NAME)?)?, key))
    })?;
    lua.set_named_registry_value(PAIRS_ITERATOR, iterator)?;

    let handles = lua.create_table()?;
    let weak_values = lua.create_table()?;
    weak_values.set("__mode", "v")?;
    handles.set_metatable(Some(weak_values))?;
    lua.set_named_registry_value(HANDLES, handles)?;
    let read = lua.create_function(|lua, (id, key): (u64, Value)| {
        backing(&handled(lua, id)?)?.raw_get::<Value>(key)
    })?;
    lua.set_named_registry_value(HOST_READ, read)?;
    let write = lua.create_function(|lua, (id, key, value): (u64, Value, Value)| {
        self::write(lua, &handled(lua, id)?, key, value)
    })?;
    lua.set_named_registry_value(HOST_WRITE, write)?;
    let host_close = lua.create_function(|lua, id: u64| close(lua, &handled(lua, id)?))?;
    lua.set_named_registry_value(HOST_CLOSE, host_close)?;

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
            let this = open_table(this, WHAT)?;
            let Value::Function(subscriber) = subscriber else {
                return Err(error(format!(
                    "{WHAT}: expected a function, got {}",
                    subscriber.type_name()
                )));
            };
            let on_close = match on_close {
                Value::Nil => None,
                Value::Function(on_close) => Some(on_close),
                other => {
                    return Err(error(format!(
                        "{WHAT}: expected a function for on_close, got {}",
                        other.type_name()
                    )));
                }
            };
            subscribe_lua(lua, this, subscriber, on_close)
        })?,
    )?;
    table_fn.set(
        "close",
        lua.create_function(|lua, values: MultiValue| {
            let [this] = args(values, "bridge.table.close")?;
            close(lua, &bridge_table(this, "bridge.table.close")?)
        })?,
    )?;

    // Built from Lua as it was when the module was built, as with `next` above. See [`HELPERS`].
    let (protect, make_batch, copy): (Function, Function, Function) =
        lua.load(HELPERS).set_name("=[bridge]").call(())?;
    lua.set_named_registry_value(COPY_CHANGES, copy)?;
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
            let (ok, handled, raised): (bool, bool, Value) = protect.call(body)?;
            match raised {
                _ if ok => Ok((true, Value::Nil)),
                // Lua calls no handler for a memory error, and gives only its message.
                raised if !handled => Err(mlua::Error::MemoryError(raised.to_string()?)),
                Value::Error(raised) if is_limit(&raised) => Err(*raised),
                raised => Ok((false, raised)),
            }
        })
    })?;
    table_fn.set("batch", make_batch.call::<Function>(run)?)?;

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

    let module = lua.create_table()?;
    module.set("table", table_fn)?;
    Ok(Value::Table(module))
}

/// The parts of `bridge.table.batch` that are Lua, since Rust can neither raise a Lua value as it
/// is nor read the value a function raised without making it a string; and the copying of a
/// batch's list for a Lua subscriber, which stays in Lua so that a long list never takes a Rust
/// reference per value.
///
/// `protect(fn)` runs `fn` and gives back whether it returned, whether a handler saw it raise
/// (Lua calls none for a memory error), and what it raised. A yield cannot reach past the Rust
/// that calls it, and that is said plainly. `batch(run)` makes `bridge.table.batch` from the Rust
/// half, raising again what `fn` raised. `copy(pending)` copies a list of changes.
const HELPERS: &str = "
    local xpcall, error = xpcall, error
    local cannot_yield = {
        ['attempt to yield across a C-call boundary'] = true,
        ['attempt to yield from outside a coroutine'] = true,
    }
    local function protect(fn)
        local handled = false
        local function handler(raised)
            handled = true
            if cannot_yield[raised] then return 'bridge.table.batch: fn cannot yield' end
            return raised
        end
        local function settle(ok, raised) return ok, handled, raised end
        return settle(xpcall(fn, handler))
    end
    local function batch(run)
        return function(...)
            local ok, raised = run(...)
            if not ok then error(raised, 0) end
        end
    end
    local function copy(pending)
        local list = {}
        for i = 1, #pending do
            local change = pending[i]
            list[i] = { key = change.key, old = change.old, new = change.new }
        end
        return list
    end
    return protect, batch, copy
";

/// Runs `body` inside a batch of `this`: every write to `this` until it returns, by any path, is
/// held back, and the outermost batch tells them all as one list, in write order, when it ends,
/// whether or not `body` failed. The list is taken before it is told, so a subscriber that writes
/// is told of its write on its own, as a write outside a batch is. A limit error from `body` is
/// what goes on, whatever the telling does, and after a memory error nothing is told.
fn batched<R>(lua: &Lua, this: &AnyUserData, body: impl FnOnce() -> Result<R>) -> Result<R> {
    let state = state_of(this)?;
    let (outermost, result) = {
        let depth = BatchDepth::enter(&state);
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

/// A batch's hold on its table's batch depth, given back when it is dropped, even by a panic.
struct BatchDepth<'a> {
    state: &'a TableState,
    outermost: bool,
}

impl<'a> BatchDepth<'a> {
    fn enter(state: &'a TableState) -> Self {
        let outermost = state.batch_depth.fetch_add(1, Ordering::Relaxed) == 0;
        BatchDepth { state, outermost }
    }
}

impl Drop for BatchDepth<'_> {
    fn drop(&mut self) {
        self.state.batch_depth.fetch_sub(1, Ordering::Relaxed);
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
        id: next_id(),
        subscribers: Mutex::new(Vec::new()),
        batch_depth: AtomicUsize::new(0),
        closed: AtomicBool::new(false),
    };
    let this = lua.create_userdata(BridgeTable(Arc::new(state)))?;
    this.set_nth_user_value(BACKING, backing)?;
    Ok(this)
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
            "{what}: expected a {TABLE_NAME}, got {}",
            other.type_name()
        ))
    })
}

/// `value` as a Bridge table's userdata that is not closed, or an error naming `what`.
fn open_table(value: Value, what: &str) -> Result<AnyUserData> {
    let this = bridge_table(value, what)?;
    if state_of(&this)?.is_closed() {
        return Err(is_closed(what));
    }
    Ok(this)
}

/// The backing table of `this`, a Bridge table's userdata from [`bridge_table`].
fn backing(this: &AnyUserData) -> Result<LuaTable> {
    this.nth_user_value(BACKING)
}

/// The Bridge table a [`Table`] with the id `id` is a handle on, if it has not been collected.
fn handled(lua: &Lua, id: u64) -> Result<AnyUserData> {
    let handles: LuaTable = lua.named_registry_value(HANDLES)?;
    handles
        .raw_get::<Option<AnyUserData>>(id)?
        .ok_or_else(collected)
}

fn collected() -> mlua::Error {
    error(format!("{TABLE_NAME}: the Bridge table has been collected"))
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
    if state.is_closed() {
        return Err(is_closed(TABLE_NAME));
    }
    let backing = backing(this)?;
    let old: Value = backing.raw_get(&key)?;
    backing.raw_set(&key, &value)?;
    if state.batch_depth.load(Ordering::Relaxed) > 0 {
        let pending = match this.nth_user_value::<Option<LuaTable>>(PENDING)? {
            Some(pending) => pending,
            None => {
                let pending = lua.create_table()?;
                this.set_nth_user_value(PENDING, &pending)?;
                pending
            }
        };
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

/// Whether `e` is not a failure but the runtime ending the run: a cancel, a timeout, or the memory
/// limit.
fn is_limit(e: &mlua::Error) -> bool {
    was_cancelled(e) || was_timed_out(e) || was_out_of_memory(e)
}

/// Tells every subscriber of `this` of `changes`, in the order they subscribed.
///
/// The list is copied first, and no lock is held while a subscriber runs, since one may write to
/// the table, which nests, or subscribe or unsubscribe. One that subscribes while this runs is not
/// in the copy, so it hears only later changes; one that unsubscribes is looked for again before
/// it is called, so it stops at once.
///
/// A batch's list is made into [`Change`]s only for a Rust subscriber, since each value of one
/// holds a slot of mlua's reference stack, and then a slice at a time, of at most [`RUST_SLICE`]:
/// a Lua subscriber's list is copied in Lua, whole.
fn tell(lua: &Lua, this: &AnyUserData, changes: Changes<'_>) -> Result<()> {
    let state = state_of(this)?;
    let subscribers: Vec<(u64, Subscriber)> = state
        .subscribers()
        .iter()
        .map(|each| (each.id, each.subscriber.clone()))
        .collect();
    if subscribers.is_empty() {
        return Ok(());
    }
    let lua_subscribers: Option<LuaTable> = this.nth_user_value(LUA_SUBSCRIBERS)?;
    for (id, subscriber) in subscribers {
        match subscriber {
            Subscriber::Rust(subscriber) => {
                if !state.is_subscribed(id) {
                    continue;
                }
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
                            if from <= len && !state.is_subscribed(id) {
                                break;
                            }
                        }
                    }
                }
            }
            Subscriber::Lua => {
                let Some(lua_subscribers) = &lua_subscribers else {
                    continue;
                };
                let Some(subscriber) = lua_subscribers.raw_get::<Option<Function>>(id)? else {
                    continue;
                };
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
                // ADR 0022: the write has succeeded whatever a subscriber does, and the others are
                // still told. Reported as Astra reports a task's error, but on standard error. A
                // limit is not the subscriber failing but the runtime ending the run, so it goes
                // on out of the write.
                match subscriber.call::<()>(list) {
                    Ok(()) => {}
                    Err(e) if is_limit(&e) => return Err(e),
                    Err(e) => eprintln!("Error running a {TABLE_NAME} subscriber: {e}"),
                }
            }
        }
    }
    Ok(())
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

/// `bridge.table.subscribe(t, fn, on_close?)`: adds `subscriber`, and `on_close` if it is given,
/// kept in user values of `this` so that the collector traces them, and gives back a handle whose
/// `unsubscribe` removes them.
fn subscribe_lua(
    lua: &Lua,
    this: AnyUserData,
    subscriber: Function,
    on_close: Option<Function>,
) -> Result<AnyUserData> {
    let state = state_of(&this)?;
    let id = next_id();
    user_table(lua, &this, LUA_SUBSCRIBERS)?.raw_set(id, subscriber)?;
    if let Some(on_close) = on_close {
        user_table(lua, &this, LUA_ON_CLOSE)?.raw_set(id, on_close)?;
    }
    state.subscribers().push(Entry {
        id,
        subscriber: Subscriber::Lua,
        on_close: None,
    });
    let handle = lua.create_userdata(LuaSubscription { state, id })?;
    handle.set_nth_user_value(SUBSCRIBED_TO, this)?;
    Ok(handle)
}

/// The table in the user value `n` of `this`, made and set there if there is none yet.
fn user_table(lua: &Lua, this: &AnyUserData, n: usize) -> Result<LuaTable> {
    match this.nth_user_value::<Option<LuaTable>>(n)? {
        Some(table) => Ok(table),
        None => {
            let table = lua.create_table()?;
            this.set_nth_user_value(n, &table)?;
            Ok(table)
        }
    }
}

/// `bridge.table.close(t)`, and [`Table::close`]: closes `this`, if it is not closed already, and
/// tells every subscriber once, in the order they subscribed, by calling its close callback, Lua's
/// and Rust's alike. Each is taken out before it is called, so one that unsubscribes another before
/// its turn keeps it from being told.
///
/// Inside a batch, the changes the batch holds back are told first, so that every write made before
/// the close is told before it; the batch then has nothing left to tell when it ends, and any write
/// after the close raises.
///
/// A Lua close callback that raises is reported, as a subscriber is, and the next is told. A limit,
/// the run cancelled or timed out or out of memory, ends the telling of Lua's; Rust's are still
/// told, since this is the only time they can be, and the limit then goes on.
fn close(lua: &Lua, this: &AnyUserData) -> Result<()> {
    let state = state_of(this)?;
    if !state.mark_closed() {
        return Ok(());
    }
    let lua_half = (|| {
        if state.batch_depth.load(Ordering::Relaxed) > 0 {
            let pending = this.nth_user_value::<Option<LuaTable>>(PENDING)?;
            this.set_nth_user_value(PENDING, Value::Nil)?;
            if let Some(pending) = pending {
                tell(lua, this, Changes::Batch(&pending))?;
            }
        }
        let on_close: Option<LuaTable> = this.nth_user_value(LUA_ON_CLOSE)?;
        this.set_nth_user_value(LUA_SUBSCRIBERS, Value::Nil)?;
        this.set_nth_user_value(LUA_ON_CLOSE, Value::Nil)?;
        Ok(on_close)
    })();
    let (mut on_close, mut failed) = match lua_half {
        Ok(on_close) => (on_close, None),
        Err(e) => (None, Some(e)),
    };
    while let Some(entry) = state.next_to_close() {
        if let Some(lua_on_close) = &on_close {
            let called = lua_on_close
                .raw_get::<Option<Function>>(entry.id)
                .and_then(|lua_on_close| lua_on_close.map_or(Ok(()), |f| f.call(())));
            match called {
                Ok(()) => {}
                Err(e) if is_limit(&e) => {
                    on_close = None;
                    failed = Some(e);
                }
                Err(e) => eprintln!("Error running a {TABLE_NAME} close callback: {e}"),
            }
        }
        entry.close_rust();
    }
    failed.map_or(Ok(()), Err)
}

/// The error an operation `what` raises on a Bridge table that is closed.
fn is_closed(what: &str) -> mlua::Error {
    error(format!("{what}: the Bridge table is closed"))
}

/// A Rust subscriber to a Bridge table.
type RustSubscriber = dyn Fn(&Lua, &[Change]) + Send + Sync;

/// What a Rust subscriber is told when the table closes, with no `Lua`, since collection and the
/// dropping of the runtime are where it may be.
type OnClose = Box<dyn FnOnce() + Send>;

/// One subscription to a Bridge table. A Rust one's close callback is here, and taken out to be
/// called; a Lua one's is a user value of the userdata, as its function is.
struct Entry {
    id: u64,
    subscriber: Subscriber,
    on_close: Option<OnClose>,
}

impl Entry {
    /// Calls the Rust close callback, if this has one, and drops the rest.
    fn close_rust(self) {
        if let Some(on_close) = self.on_close {
            on_close();
        }
    }
}

/// One of a Bridge table's subscribers. A Lua one's function is not here but in the userdata's
/// user value, under the same id, for the reasons the module's header gives.
#[derive(Clone)]
enum Subscriber {
    Rust(Arc<RustSubscriber>),
    Lua,
}

/// The part of a Bridge table that is not Lua's, shared between its userdata and the host's
/// handles. It holds no Lua value, for the reasons the module's header gives: the backing table
/// and the Lua subscribers' functions are user values of the userdata, and so are the changes a
/// batch holds back, and the Lua subscribers' close callbacks.
struct TableState {
    /// The key the host's handles find the userdata by, in the table at [`HANDLES`].
    id: u64,
    /// Every subscriber, Lua's and Rust's, in the order they subscribed.
    subscribers: Mutex<Vec<Entry>>,
    /// How many batches of this table are open, one inside another. Only the Lua thread, under the
    /// Lua lock, reads or writes it.
    batch_depth: AtomicUsize,
    /// Whether the table has been closed, from either side or by being collected.
    closed: AtomicBool,
}

impl TableState {
    /// The subscribers, locked. Never held across a call to one.
    fn subscribers(&self) -> MutexGuard<'_, Vec<Entry>> {
        lock(&self.subscribers)
    }

    fn is_subscribed(&self, id: u64) -> bool {
        self.subscribers().iter().any(|each| each.id == id)
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    /// Marks the table closed, and gives whether it was open until now, so that only one close
    /// tells the subscribers.
    fn mark_closed(&self) -> bool {
        !self.closed.swap(true, Ordering::SeqCst)
    }

    /// The first subscriber left, taken out, to be told of the close with the lock released.
    fn next_to_close(&self) -> Option<Entry> {
        let mut subscribers = self.subscribers();
        (!subscribers.is_empty()).then(|| subscribers.remove(0))
    }

    /// Tells the Rust subscribers of the close, in the order they subscribed, and drops the rest.
    fn close_rust(&self) {
        while let Some(entry) = self.next_to_close() {
            entry.close_rust();
        }
    }

    /// Removes the subscriber `id`. It is dropped once the lock is released, since a Rust
    /// subscriber may own a [`Subscription`] to this same table, whose drop takes the lock.
    fn unsubscribe(&self, id: u64) {
        let removed = {
            let mut subscribers = self.subscribers();
            let at = subscribers.iter().position(|each| each.id == id);
            at.map(|at| subscribers.remove(at))
        };
        drop(removed);
    }
}

/// The userdata Lua holds a Bridge table as. Its first user value is the backing table, its
/// second, once it has had a Lua subscriber, the Lua subscribers, its third, while a batch is open
/// and has been written to, the changes the batch holds back, and its fourth, once a Lua subscriber
/// has given one, the Lua close callbacks.
struct BridgeTable(Arc<TableState>);

impl UserData for BridgeTable {
    fn add_fields<F: mlua::UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field(MetaMethod::Type, TABLE_NAME);
    }

    fn add_methods<M: mlua::UserDataMethods<Self>>(methods: &mut M) {
        // A script can take these out of the metatable and call them with something else, so each
        // checks what it was given.
        methods.add_meta_function(MetaMethod::Index, |_, (this, key): (Value, Value)| {
            backing(&bridge_table(this, TABLE_NAME)?)?.raw_get::<Value>(key)
        });
        methods.add_meta_function(
            MetaMethod::NewIndex,
            |lua, (this, key, value): (Value, Value, Value)| {
                write(lua, &bridge_table(this, TABLE_NAME)?, key, value)
            },
        );
        methods.add_meta_function(MetaMethod::Len, |_, this: Value| {
            Ok(backing(&bridge_table(this, TABLE_NAME)?)?.raw_len())
        });
        methods.add_meta_function(MetaMethod::Pairs, |lua, this: Value| {
            let iterator: Function = lua.named_registry_value(PAIRS_ITERATOR)?;
            Ok((iterator, bridge_table(this, TABLE_NAME)?, Value::Nil))
        });
        methods.add_meta_function(MetaMethod::ToString, |_, this: AnyUserData| {
            Ok(format!(
                "{TABLE_NAME}: {:p}",
                Value::UserData(this).to_pointer()
            ))
        });
    }
}

/// Collection closes a Bridge table nobody closed, and so does dropping the runtime, which collects
/// everything. Only the Rust close callbacks are told: a Lua one is being collected with the table,
/// and this has no `Lua` to call it with (ADR 0022).
impl Drop for BridgeTable {
    fn drop(&mut self) {
        if self.0.mark_closed() {
            self.0.close_rust();
        }
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
        methods.add_function("unsubscribe", |_, mut values: MultiValue| {
            const WHAT: &str = "bridge.subscription.unsubscribe";
            let handle = match values.pop_front().unwrap_or(Value::Nil) {
                Value::UserData(handle) if handle.is::<LuaSubscription>() => handle,
                other => {
                    return Err(error(format!(
                        "{WHAT}: expected a {SUBSCRIPTION_NAME}, got {}",
                        other.type_name()
                    )));
                }
            };
            let [] = args(values, WHAT)?;
            let (state, id) = {
                let this = handle.borrow::<LuaSubscription>()?;
                (Arc::clone(&this.state), this.id)
            };
            state.unsubscribe(id);
            let table: AnyUserData = handle.nth_user_value(SUBSCRIBED_TO)?;
            for n in [LUA_SUBSCRIBERS, LUA_ON_CLOSE] {
                if let Some(lua_table) = table.nth_user_value::<Option<LuaTable>>(n)? {
                    lua_table.raw_set(id, Value::Nil)?;
                }
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
/// whether or not the host still has a handle (ADR 0022), and so closes it. Writing through a
/// handle on one that is closed returns an error, and so does reading or writing through one on
/// one that has been collected, or whose `Lua` is gone.
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
        self.call(HOST_READ, (self.id, key))
    }

    /// Writes `value` at `key`, as a script's `t[key] = value` does, and tells every subscriber,
    /// Lua's included, before it returns. The write and the telling are one call into Lua, so they
    /// hold the Lua lock throughout, from whichever thread this is called.
    ///
    /// Writing to one that is closed returns an error, without calling into Lua, so a close
    /// callback may try it even while the table is being collected.
    pub fn set(&self, key: impl IntoLua, value: impl IntoLua) -> Result<()> {
        match self.state.upgrade() {
            Some(state) if state.is_closed() => Err(is_closed(TABLE_NAME)),
            _ => self.call(HOST_WRITE, (self.id, key, value)),
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
            Some(state) if !state.is_closed() => self.call(HOST_CLOSE, self.id),
            _ => Ok(()),
        }
    }

    fn add(&self, subscriber: Arc<RustSubscriber>, on_close: Option<OnClose>) -> Subscription {
        let id = next_id();
        let entry = Entry {
            id,
            subscriber: Subscriber::Rust(subscriber),
            on_close,
        };
        // Whether it is closed is asked under the lock that a close takes each subscriber out
        // under, so that the close is sure to find one added while it runs.
        let refused = match self.state.upgrade() {
            Some(state) => {
                let mut subscribers = state.subscribers();
                if state.is_closed() {
                    Some(entry)
                } else {
                    subscribers.push(entry);
                    None
                }
            }
            None => Some(entry),
        };
        if let Some(on_close) = refused.and_then(|entry| entry.on_close) {
            on_close();
        }
        Subscription {
            state: Weak::clone(&self.state),
            id,
        }
    }

    /// Calls the function `rust_half` kept at `name`, giving back its own error rather than one
    /// wrapped as raised in a callback.
    fn call<R: FromLuaMulti>(&self, name: &str, args: impl IntoLuaMulti) -> Result<R> {
        let lua = self.lua.try_upgrade().ok_or_else(collected)?;
        let function: Function = lua.named_registry_value(name)?;
        function.call(args).map_err(|e| match e {
            mlua::Error::CallbackError { cause, .. } => Arc::unwrap_or_clone(cause),
            e => e,
        })
    }
}

impl fmt::Debug for Table {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Table")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl FromLua for Table {
    fn from_lua(value: Value, lua: &Lua) -> Result<Self> {
        let this = as_bridge_table(value).map_err(|other| mlua::Error::FromLuaConversionError {
            from: other.type_name(),
            to: "bridge::Table".to_owned(),
            message: Some(format!("expected a {TABLE_NAME}")),
        })?;
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

/// A Rust subscription to a Bridge table, from [`Table::subscribe`] or
/// [`Table::subscribe_with_close`]. Dropping it unsubscribes, at once: a subscriber is not called
/// again once its subscription is dropped, even by a write it is being told of, and its close
/// callback is not called.
#[must_use = "dropping a Subscription unsubscribes"]
pub struct Subscription {
    state: Weak<TableState>,
    id: u64,
}

impl fmt::Debug for Subscription {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Subscription")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
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
