//! What every Bridge object has, whatever it holds: its subscribers, Lua's and Rust's, in a
//! [`Core`]; the telling of a change to them, in [`tell`]; its closing, in [`close`] and
//! [`Core::close_by_collection`]; the handle `subscribe` gives Lua; and the host's weak handle,
//! [`Handle`], and [`Subscription`]. What a change is, and how a subscriber is called with one, is
//! each object's own, and given to these by it.
//!
//! An object's userdata keeps its Lua subscribers in the user value [`LUA_SUBSCRIBERS`], and their
//! close callbacks in [`LUA_ON_CLOSE`], by subscription id, for the reasons the module's header
//! gives. The rest of its user values are its own.

use std::cell::Cell;
use std::fmt;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};

use mlua::{
    AnyUserData, FromLuaMulti, Function, IntoLuaMulti, Lua, MetaMethod, MultiValue,
    Table as LuaTable, UserData, Value, WeakLua,
};

use super::{Depth, Result, args, error, is_limit};
use crate::lock::lock;

/// What a kind of Bridge object is called: its `__name`, and its name in prose.
pub(super) struct Kind {
    pub name: &'static str,
    pub noun: &'static str,
}

/// The user value of a Bridge object's userdata that holds its Lua subscribers, by subscription
/// id, once it has had one.
pub(super) const LUA_SUBSCRIBERS: usize = 2;

/// The user value of a Bridge object's userdata that holds its Lua subscribers' close callbacks,
/// by subscription id, once one has been given.
pub(super) const LUA_ON_CLOSE: usize = 3;

/// The user value of a subscription handle's userdata that holds the Bridge object it is of.
const SUBSCRIBED_TO: usize = 1;

/// The name of the handle `subscribe` gives back.
const SUBSCRIPTION_NAME: &str = "bridge.subscription";

/// Where [`register`] keeps the weak-valued table through which a [`Handle`] finds its userdata:
/// by the id in its [`Core`], so that the host's hold does not keep a Bridge object alive.
const HANDLES: &str = "avarice.bridge.handles";

/// Gives out the ids of Bridge objects and of subscriptions, which are never reused.
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

pub(super) fn next_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

/// Keeps in the registry what the host's handles need.
pub(super) fn register(lua: &Lua) -> Result<()> {
    let handles = lua.create_table()?;
    let weak_values = lua.create_table()?;
    weak_values.set("__mode", "v")?;
    handles.set_metatable(Some(weak_values))?;
    lua.set_named_registry_value(HANDLES, handles)
}

/// What a Rust subscriber is told when the object closes, with no `Lua`, since collection and the
/// dropping of the runtime are where it may be.
pub(super) type OnClose = Box<dyn FnOnce() + Send>;

/// One subscription to a Bridge object. A Rust one's close callback is here, and taken out to be
/// called; a Lua one's is a user value of the userdata, as its function is.
struct Entry<F: ?Sized> {
    id: u64,
    subscriber: Subscriber<F>,
    on_close: Option<OnClose>,
}

impl<F: ?Sized> Entry<F> {
    /// Calls the Rust close callback, if this has one, and drops the rest.
    fn call_on_close(self) {
        if let Some(on_close) = self.on_close {
            on_close();
        }
    }
}

/// One of a Bridge object's subscribers, a Rust one being an `F`. A Lua one's function is not
/// here but in the userdata's user value, under the same id, for the reasons the module's header
/// gives.
enum Subscriber<F: ?Sized> {
    Rust(Arc<F>),
    Lua,
}

impl<F: ?Sized> Clone for Subscriber<F> {
    fn clone(&self) -> Self {
        match self {
            Subscriber::Rust(subscriber) => Subscriber::Rust(Arc::clone(subscriber)),
            Subscriber::Lua => Subscriber::Lua,
        }
    }
}

/// The part of a Bridge object that is not Lua's and that every kind has, shared between its
/// userdata and the host's handles. It holds no Lua value, for the reasons the module's header
/// gives. Its Rust subscribers are each an `F`.
pub(super) struct Core<F: ?Sized> {
    pub kind: &'static Kind,
    /// The key the host's handles find the userdata by, in the table at [`HANDLES`].
    pub id: u64,
    /// Every subscriber, Lua's and Rust's, in the order they subscribed.
    subscribers: Mutex<Vec<Entry<F>>>,
    /// Whether the object has been closed, from either side or by being collected.
    closed: AtomicBool,
    /// How many tellings of a change to this object are running, one inside another. Only the Lua
    /// thread, under the Lua lock, reads or writes it.
    telling: AtomicUsize,
    /// Whether a close made while a change was being told waits to tell its subscribers.
    close_waiting: AtomicBool,
}

impl<F: ?Sized> Core<F> {
    pub fn new(kind: &'static Kind) -> Self {
        Core {
            kind,
            id: next_id(),
            subscribers: Mutex::new(Vec::new()),
            closed: AtomicBool::new(false),
            telling: AtomicUsize::new(0),
            close_waiting: AtomicBool::new(false),
        }
    }

    /// The subscribers, locked. Never held across a call to one.
    fn subscribers(&self) -> MutexGuard<'_, Vec<Entry<F>>> {
        lock(&self.subscribers)
    }

    pub fn is_subscribed(&self, id: u64) -> bool {
        self.subscribers().iter().any(|each| each.id == id)
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    /// Marks the object closed, and gives whether it was open until now, so that only one close
    /// tells the subscribers.
    fn mark_closed(&self) -> bool {
        !self.closed.swap(true, Ordering::SeqCst)
    }

    /// The first subscriber left, taken out, to be told of the close with the lock released.
    fn next_to_close(&self) -> Option<Entry<F>> {
        let mut subscribers = self.subscribers();
        (!subscribers.is_empty()).then(|| subscribers.remove(0))
    }

    /// Takes out each subscriber in turn, in the order they subscribed, and gives it to `tell`,
    /// with the lock released, until none is left.
    fn drain(&self, mut tell: impl FnMut(Entry<F>)) {
        while let Some(entry) = self.next_to_close() {
            tell(entry);
        }
    }

    /// Removes the subscriber `id`. It is dropped once the lock is released, since a Rust
    /// subscriber may own a [`Subscription`] to this same object, whose drop takes the lock.
    fn remove(&self, id: u64) {
        let removed = {
            let mut subscribers = self.subscribers();
            let at = subscribers.iter().position(|each| each.id == id);
            at.map(|at| subscribers.remove(at))
        };
        drop(removed);
    }

    /// Adds a Rust subscriber, and gives its id. If the object is closed, it is not added, and
    /// `on_close` is called at once.
    fn add_rust(&self, subscriber: Arc<F>, on_close: Option<OnClose>) -> u64 {
        let entry = Entry {
            id: next_id(),
            subscriber: Subscriber::Rust(subscriber),
            on_close,
        };
        let id = entry.id;
        // Whether it is closed is asked under the lock that a close takes each subscriber out
        // under, so that the close is sure to find one added while it runs.
        let refused = {
            let mut subscribers = self.subscribers();
            if self.is_closed() {
                Some(entry)
            } else {
                subscribers.push(entry);
                None
            }
        };
        if let Some(entry) = refused {
            entry.call_on_close();
        }
        id
    }

    /// The close that collection makes of an object nobody closed, and that dropping the runtime
    /// makes, which collects everything, from the userdata's `Drop`. Only the Rust close callbacks
    /// are told: a Lua one is being collected with the object, and this has no `Lua` to call it
    /// with (ADR 0022).
    ///
    /// It runs inside Lua's collection, or its closing of the state, where neither hooks nor
    /// limits run, so while it does a [`Handle`] refuses to call into Lua, and a close callback
    /// that panics is reported rather than unwinding through Lua's C code.
    pub fn close_by_collection(&self) {
        // A close that was waiting for a telling a panic unwound out of has yet to be told.
        if !self.mark_closed() && !self.close_waiting.swap(false, Ordering::SeqCst) {
            return;
        }
        let _collecting = Collecting::enter();
        let name = self.kind.name;
        self.drain(|entry| {
            let called = std::panic::catch_unwind(AssertUnwindSafe(|| entry.call_on_close()));
            if let Err(panic) = called {
                let message = panic
                    .downcast_ref::<&str>()
                    .copied()
                    .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
                    .unwrap_or("a panic");
                eprintln!("A {name} close callback panicked during collection: {message}");
            }
        });
    }
}

/// What a [`Subscription`] and the handle `subscribe` gives Lua unsubscribe from, whatever kind
/// of object it is.
pub(super) trait Subscribed: Send + Sync {
    fn unsubscribe(&self, id: u64);
}

impl<F: ?Sized + Send + Sync> Subscribed for Core<F> {
    fn unsubscribe(&self, id: u64) {
        self.remove(id);
    }
}

/// Tells every subscriber of `this`, whose [`Core`] is `core`, of a change, in the order they
/// subscribed: a Rust one by `rust`, given its id and itself, and a Lua one by calling it with what
/// `lua_args` gives.
///
/// The list is copied first, and no lock is held while a subscriber runs, since one may write to
/// the object, which nests, or subscribe or unsubscribe. One that subscribes while this runs is not
/// in the copy, so it hears only later changes; one that unsubscribes is looked for again before
/// it is called, so it stops at once.
///
/// A close made while this runs, by a subscriber or by a write nested in one, waits for it: the
/// outermost telling of the object tells the close once every subscriber has heard the change.
pub(super) fn tell<F: ?Sized>(
    this: &AnyUserData,
    core: &Core<F>,
    rust: impl FnMut(u64, &F) -> Result<()>,
    lua_args: impl FnMut() -> Result<MultiValue>,
) -> Result<()> {
    let lua_subscribers = this.nth_user_value(LUA_SUBSCRIBERS)?;
    tell_taken(this, core, lua_subscribers, rust, lua_args)
}

/// [`tell`], to the Lua subscribers `lua_subscribers`, which [`close`] took out of the user value
/// [`LUA_SUBSCRIBERS`] before it gave them to its `flush`.
pub(super) fn tell_taken<F: ?Sized>(
    this: &AnyUserData,
    core: &Core<F>,
    lua_subscribers: Option<LuaTable>,
    rust: impl FnMut(u64, &F) -> Result<()>,
    lua_args: impl FnMut() -> Result<MultiValue>,
) -> Result<()> {
    let (outermost, told) = {
        let depth = Depth::enter(&core.telling);
        (
            depth.outermost,
            tell_each(core, lua_subscribers, rust, lua_args),
        )
    };
    if outermost && core.close_waiting.swap(false, Ordering::SeqCst) {
        let (on_close, failed) = match (told, take_user_value(this, LUA_ON_CLOSE)) {
            (Ok(()), Ok(on_close)) => (on_close, None),
            (Err(e), _) | (_, Err(e)) => (None, Some(e)),
        };
        return tell_close(core, on_close, failed);
    }
    told
}

/// The body of [`tell_taken`], with no close waiting on it.
fn tell_each<F: ?Sized>(
    core: &Core<F>,
    lua_subscribers: Option<LuaTable>,
    mut rust: impl FnMut(u64, &F) -> Result<()>,
    mut lua_args: impl FnMut() -> Result<MultiValue>,
) -> Result<()> {
    let subscribers: Vec<(u64, Subscriber<F>)> = core
        .subscribers()
        .iter()
        .map(|each| (each.id, each.subscriber.clone()))
        .collect();
    for (id, subscriber) in subscribers {
        match subscriber {
            Subscriber::Rust(subscriber) => {
                if core.is_subscribed(id) {
                    rust(id, &subscriber)?;
                }
            }
            Subscriber::Lua => {
                let Some(lua_subscribers) = &lua_subscribers else {
                    continue;
                };
                let Some(subscriber) = lua_subscribers.raw_get::<Option<Function>>(id)? else {
                    continue;
                };
                // ADR 0022: the write has succeeded whatever a subscriber does, and the others are
                // still told. Reported as Astra reports a task's error, but on standard error. A
                // limit is not the subscriber failing but the runtime ending the run, so it goes
                // on out of the write.
                match subscriber.call::<()>(lua_args()?) {
                    Ok(()) => {}
                    Err(e) if is_limit(&e) => return Err(e),
                    Err(e) => eprintln!("Error running a {} subscriber: {e}", core.kind.name),
                }
            }
        }
    }
    Ok(())
}

/// The arguments `subscribe(fn, on_close?)` was given, checked, as of `what`.
pub(super) fn subscriber_args(
    subscriber: Value,
    on_close: Value,
    what: &str,
) -> Result<(Function, Option<Function>)> {
    let Value::Function(subscriber) = subscriber else {
        return Err(error(format!(
            "{what}: expected a function, got {}",
            subscriber.type_name()
        )));
    };
    let on_close = match on_close {
        Value::Nil => None,
        Value::Function(on_close) => Some(on_close),
        other => {
            return Err(error(format!(
                "{what}: expected a function for on_close, got {}",
                other.type_name()
            )));
        }
    };
    Ok((subscriber, on_close))
}

/// Subscribes from Lua, as of `what`: adds `subscriber`, and `on_close` if it is given, kept in
/// user values of `this`, whose state is `state`, so that the collector traces them, and gives
/// back a handle whose `unsubscribe` removes them. `core` finds its [`Core`] in the state. On an
/// object that is closed it is an error.
pub(super) fn subscribe_lua<S, F>(
    lua: &Lua,
    this: AnyUserData,
    state: Arc<S>,
    core: impl FnOnce(&S) -> &Core<F>,
    (subscriber, on_close): (Function, Option<Function>),
    what: &str,
) -> Result<AnyUserData>
where
    S: Subscribed + 'static,
    F: ?Sized,
{
    let core = core(&state);
    if core.is_closed() {
        return Err(is_closed(core.kind, what));
    }
    let id = next_id();
    user_table(lua, &this, LUA_SUBSCRIBERS)?.raw_set(id, subscriber)?;
    if let Some(on_close) = on_close {
        user_table(lua, &this, LUA_ON_CLOSE)?.raw_set(id, on_close)?;
    }
    core.subscribers().push(Entry {
        id,
        subscriber: Subscriber::Lua,
        on_close: None,
    });
    let handle = lua.create_userdata(LuaSubscription { state, id })?;
    handle.set_nth_user_value(SUBSCRIBED_TO, this)?;
    Ok(handle)
}

/// The table in the user value `n` of `this`, made and set there if there is none yet.
pub(super) fn user_table(lua: &Lua, this: &AnyUserData, n: usize) -> Result<LuaTable> {
    match this.nth_user_value::<Option<LuaTable>>(n)? {
        Some(table) => Ok(table),
        None => {
            let table = lua.create_table()?;
            this.set_nth_user_value(n, &table)?;
            Ok(table)
        }
    }
}

/// The table in the user value `n` of `this`, if there is one, taken out.
pub(super) fn take_user_value(this: &AnyUserData, n: usize) -> Result<Option<LuaTable>> {
    let value = this.nth_user_value(n)?;
    this.set_nth_user_value(n, Value::Nil)?;
    Ok(value)
}

/// Closes `this`, whose [`Core`] is `core`, if it is not closed already, and tells every
/// subscriber once, in the order they subscribed, by calling its close callback, Lua's and Rust's
/// alike. Each is taken out before it is called, so one that unsubscribes another before its turn
/// keeps it from being told.
///
/// `flush` is given the Lua subscribers, taken out, and tells them, and the Rust ones, of whatever
/// the object has yet to tell, before the close is told.
///
/// A Lua close callback that raises is reported, as a subscriber is, and the next is told. A limit,
/// the run cancelled or timed out or out of memory, ends the telling of Lua's; Rust's are still
/// told, since this is the only time they can be, and the limit then goes on.
///
/// While a change is being told, the close takes effect for writes at once, but its close
/// callbacks wait until the change has reached every subscriber (see [`tell`]), so that the close
/// is the last a subscriber hears.
pub(super) fn close<F: ?Sized>(
    this: &AnyUserData,
    core: &Core<F>,
    flush: impl FnOnce(Option<LuaTable>) -> Result<()>,
) -> Result<()> {
    if !core.mark_closed() {
        return Ok(());
    }
    // Taken out before anything that a limit could fail, since after one even this would.
    let waits = core.telling.load(Ordering::Relaxed) > 0;
    let taken = (|| {
        let on_close = match waits {
            true => None,
            false => take_user_value(this, LUA_ON_CLOSE)?,
        };
        Ok((on_close, take_user_value(this, LUA_SUBSCRIBERS)?))
    })();
    let (on_close, flushed) = match taken {
        Ok((on_close, lua_subscribers)) => (on_close, flush(lua_subscribers)),
        Err(e) => (None, Err(e)),
    };
    if waits {
        core.close_waiting.store(true, Ordering::SeqCst);
        return flushed;
    }
    tell_close(core, on_close, flushed.err())
}

/// Tells every subscriber that the object has closed, in the order they subscribed, by
/// `on_close`, the Lua close callbacks [`close`] took out, and by their own: the part of a close
/// that waits for a change being told. `failed` is how what came before failed, if it did, which
/// goes on, and after which no Lua close callback is called.
fn tell_close<F: ?Sized>(
    core: &Core<F>,
    on_close: Option<LuaTable>,
    failed: Option<mlua::Error>,
) -> Result<()> {
    let mut failed = failed;
    let mut on_close = on_close.filter(|_| failed.is_none());
    core.drain(|entry| {
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
                Err(e) => eprintln!("Error running a {} close callback: {e}", core.kind.name),
            }
        }
        entry.call_on_close();
    });
    failed.map_or(Ok(()), Err)
}

/// The error an operation `what` raises on a Bridge object of `kind` that is closed.
pub(super) fn is_closed(kind: &Kind, what: &str) -> mlua::Error {
    error(format!("{what}: the {} is closed", kind.noun))
}

thread_local! {
    /// How many closes by collection are telling their close callbacks on this thread.
    static COLLECTING: Cell<usize> = const { Cell::new(0) };
}

/// A close by collection's mark on its thread, taken off when it is dropped, even by a panic.
struct Collecting;

impl Collecting {
    fn enter() -> Self {
        COLLECTING.with(|collecting| collecting.set(collecting.get() + 1));
        Collecting
    }

    fn is_running() -> bool {
        COLLECTING.with(Cell::get) > 0
    }
}

impl Drop for Collecting {
    fn drop(&mut self) {
        COLLECTING.with(|collecting| collecting.set(collecting.get() - 1));
    }
}

/// The handle `subscribe` gives Lua back. Its user value is the Bridge object, whose user value
/// holds the subscriber's function.
struct LuaSubscription {
    state: Arc<dyn Subscribed>,
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
            let object: AnyUserData = handle.nth_user_value(SUBSCRIBED_TO)?;
            for n in [LUA_SUBSCRIBERS, LUA_ON_CLOSE] {
                if let Some(lua_table) = object.nth_user_value::<Option<LuaTable>>(n)? {
                    lua_table.raw_set(id, Value::Nil)?;
                }
            }
            Ok(())
        });
    }
}

/// The host's weak hold on a Bridge object's userdata: its id, looked up in the weak-valued table
/// at [`HANDLES`], and its `Lua`, held weakly. Each kind's own handle calls through it.
#[derive(Clone)]
pub(super) struct Handle {
    pub id: u64,
    kind: &'static Kind,
    lua: WeakLua,
}

impl Handle {
    /// A handle on `this`, whose [`Core`] is `core`, entered in the table at [`HANDLES`].
    pub fn new<F: ?Sized>(lua: &Lua, this: &AnyUserData, core: &Core<F>) -> Result<Self> {
        let handles: LuaTable = lua.named_registry_value(HANDLES)?;
        handles.raw_set(core.id, this)?;
        Ok(Handle {
            id: core.id,
            kind: core.kind,
            lua: lua.weak(),
        })
    }

    /// Calls the function its kind kept in the registry at `name`, giving back its own error
    /// rather than one wrapped as raised in a callback.
    pub fn call<R: FromLuaMulti>(&self, name: &str, args: impl IntoLuaMulti) -> Result<R> {
        let Kind { name: kind, noun } = self.kind;
        if Collecting::is_running() {
            return Err(error(format!(
                "{kind}: a {noun} cannot be used from a close callback run by collection"
            )));
        }
        let lua = self.lua.try_upgrade().ok_or_else(|| collected(self.kind))?;
        let function: Function = lua.named_registry_value(name)?;
        function.call(args).map_err(|e| match e {
            mlua::Error::CallbackError { cause, .. } => Arc::unwrap_or_clone(cause),
            e => e,
        })
    }
}

/// The Bridge object of `kind` that a [`Handle`] with the id `id` is a handle on, if it has not
/// been collected.
pub(super) fn handled(lua: &Lua, id: u64, kind: &Kind) -> Result<AnyUserData> {
    let handles: LuaTable = lua.named_registry_value(HANDLES)?;
    handles
        .raw_get::<Option<AnyUserData>>(id)?
        .ok_or_else(|| collected(kind))
}

fn collected(kind: &Kind) -> mlua::Error {
    error(format!(
        "{}: the {} has been collected",
        kind.name, kind.noun
    ))
}

/// Adds a Rust subscriber to the object whose state is `state`, giving back the [`Subscription`]
/// that removes it: `core` finds its [`Core`] in the state. If the object is closed, or has been
/// collected, `on_close` is called at once, before this returns.
pub(super) fn add_rust<S, F>(
    state: &Weak<S>,
    core: impl FnOnce(&S) -> &Core<F>,
    subscriber: Arc<F>,
    on_close: Option<OnClose>,
) -> Subscription
where
    S: Subscribed + 'static,
    F: ?Sized,
{
    let id = match state.upgrade() {
        Some(upgraded) => core(&upgraded).add_rust(subscriber, on_close),
        None => {
            if let Some(on_close) = on_close {
                on_close();
            }
            next_id()
        }
    };
    let state: Weak<dyn Subscribed> = Weak::clone(state) as Weak<dyn Subscribed>;
    Subscription { state, id }
}

/// A Rust subscription to a Bridge object, from its handle's `subscribe` or `subscribe_with_close`.
/// Dropping it unsubscribes, at once: a subscriber is not called again once its subscription is
/// dropped, even by a write it is being told of, and its close callback is not called.
#[must_use = "dropping a Subscription unsubscribes"]
pub struct Subscription {
    state: Weak<dyn Subscribed>,
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
