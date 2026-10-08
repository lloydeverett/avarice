//! `tidings` is original to avarice, not derived from Astra: ADR 0021 records why. It binds
//! [tidings](https://github.com/lloydeverett/tidings) to Lua: **tidings Stores** on the
//! filesystem, SQLite or memory, read directly, written only by committing a Staging, and reporting
//! every Change on a feed with one reader.
//!
//! The binding is thin and follows tidings' names, with these Lua-shaped differences. A Conflict
//! is returned, as `nil` and a table naming its Paths, since a script branches on it; every other
//! failure raises (ADR 0017). A Pending Commit is returned like a finished one, with `pending` set,
//! since it has happened. A Revision is a string, since it reads back from text; a Prefix Revision
//! is a userdata, since it does not. Times are `datetime`'s Timestamps. And a Store and its feed
//! can each be closed, where Rust would drop them.
//!
//! A Store on the filesystem or SQLite runs tidings' tasks on the executor for as long as it is
//! open, so it counts as a task, and `avarice` waits for it to be closed before exiting. If
//! `abort_tasks` ends those tasks the Store would carry on without seeing Changes, so each such
//! Store also runs a sentinel task, whose end without a `close` marks the Store as aborted: it
//! then raises, and its feed gives one Resync and ends.
//!
//! Nothing here sets a global. [`rust_half`] builds the module's table, and `modules.rs` hands it
//! to `lua/tidings.lua`, which returns it.

use std::ffi::OsString;
use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex, PoisonError};

use mlua::{
    AnyUserData, IntoLuaMulti, Lua, LuaString, MetaMethod, MultiValue, Table, UserData,
    UserDataFields, UserDataMethods, Value,
};
use tidings::{ChangeKind, FeedItem, Origin, Precondition, Revision};
use tokio::sync::Notify;
use tokio::task::AbortHandle;

use super::datetime::Timestamp;

type Result<T> = mlua::Result<T>;

fn error(message: String) -> mlua::Error {
    mlua::Error::runtime(message)
}

/// A failure of tidings', raised, naming what the script called.
fn failed(what: &str, e: tidings::Error) -> mlua::Error {
    error(format!("tidings: {what}: {e}"))
}

/// Builds the table `lua/tidings.lua` returns.
pub fn rust_half(lua: &Lua) -> Result<Value> {
    let module = lua.create_table()?;
    module.set(
        "open_fs",
        lua.create_async_function(|lua, location: Value| async move {
            let what = "tidings.open_fs";
            let location = location_arg(&location, what)?;
            let shown = location.display().to_string();
            let (store, feed) = tidings::Store::open_fs(location, tidings::FsOptions::default())
                .await
                .map_err(|e| failed(what, e))?;
            opened(&lua, store, feed, format!("fs, {shown}"), true)
        })?,
    )?;
    module.set(
        "open_sqlite",
        lua.create_async_function(|lua, location: Value| async move {
            let what = "tidings.open_sqlite";
            let location = location_arg(&location, what)?;
            let shown = location.display().to_string();
            let (store, feed) =
                tidings::Store::open_sqlite(location, tidings::SqliteOptions::default())
                    .await
                    .map_err(|e| failed(what, e))?;
            opened(&lua, store, feed, format!("sqlite, {shown}"), true)
        })?,
    )?;
    module.set(
        "open_memory",
        lua.create_function(|lua, ()| {
            let (store, feed) = tidings::Store::open_memory();
            // The memory Backend runs no tasks, so there is nothing for `abort_tasks` to end.
            opened(lua, store, feed, "memory".to_owned(), false)
        })?,
    )?;
    module.set(
        "detect",
        lua.create_async_function(|_, location: Value| async move {
            let what = "tidings.detect";
            let location = location_arg(&location, what)?;
            let kind = tidings::Store::detect(location)
                .await
                .map_err(|e| failed(what, e))?;
            Ok(kind.map(|kind| kind.to_string()))
        })?,
    )?;
    module.set(
        "staging",
        lua.create_function(|_, ()| Ok(Staging(StdMutex::new(Some(tidings::Staging::new())))))?,
    )?;
    Ok(Value::Table(module))
}

/// The Store and its feed, as `open_*` give them to Lua. `tasks` says whether the Backend runs
/// tasks on the executor, which a sentinel then watches for `abort_tasks`.
fn opened(
    lua: &Lua,
    store: tidings::Store,
    feed: tidings::ChangeFeed,
    description: String,
    tasks: bool,
) -> Result<MultiValue> {
    let health = Arc::new(Health::default());
    let sentinel = tasks.then(|| Sentinel::spawn(&health));
    let store = Store(Arc::new(StoreState {
        store: StdMutex::new(Some(store)),
        health: Arc::clone(&health),
        sentinel,
        description,
    }));
    let feed = Feed(Arc::new(FeedState {
        feed: tokio::sync::Mutex::new(Some(feed)),
        closed: AtomicBool::new(false),
        closing: Notify::new(),
        health,
    }));
    (store, feed).into_lua_multi(lua)
}

// -- Arguments ---------------------------------------------------------------------------------

/// A string argument, refusing what Lua would coerce: a number is not a Path or contents.
fn string_arg(value: &Value, what: &str, name: &str) -> Result<LuaString> {
    match value {
        Value::String(s) => Ok(s.clone()),
        other => Err(error(format!(
            "tidings: {what}: {name} must be a string, not a {}",
            other.type_name()
        ))),
    }
}

/// A Path or Prefix: text, since tidings' Paths are UTF-8.
fn text_arg(value: &Value, what: &str, name: &str) -> Result<String> {
    let s = string_arg(value, what, name)?;
    match s.to_str() {
        Ok(text) => Ok(text.to_owned()),
        Err(_) => Err(error(format!("tidings: {what}: {name} is not valid UTF-8"))),
    }
}

/// A Location, as the operating system is given it: its exact bytes on Unix, and elsewhere the
/// string if it is valid Unicode (ADR 0015).
fn location_arg(value: &Value, what: &str) -> Result<PathBuf> {
    let s = string_arg(value, what, "the Location")?;
    let bytes = s.as_bytes();
    if bytes.contains(&0) {
        return Err(error(format!(
            "tidings: {what}: the Location holds a NUL byte"
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        Ok(OsString::from_vec(bytes.to_vec()).into())
    }
    #[cfg(not(unix))]
    {
        match std::str::from_utf8(&bytes) {
            Ok(text) => Ok(OsString::from(text).into()),
            Err(_) => Err(error(format!(
                "tidings: {what}: the Location is not valid Unicode, which this platform needs"
            ))),
        }
    }
}

/// A Revision, from the text a File or a Commit gave.
fn revision_arg(value: &Value, what: &str) -> Result<Revision> {
    let text = text_arg(value, what, "a Revision")?;
    text.parse()
        .map_err(|_| error(format!("tidings: {what}: {text:?} is not a Revision")))
}

/// The Path or Prefix a reading method takes, or the whole Store for `nil`.
fn prefix_or_whole(value: &Value, what: &str) -> Result<String> {
    match value {
        Value::Nil => Ok(String::new()),
        other => text_arg(other, what, "the Prefix"),
    }
}

/// The Precondition in a write's or a delete's options table: `if_absent` or `if_revision`, or
/// none.
fn precondition_option(options: &Value, what: &str) -> Result<Precondition> {
    let table = match options {
        Value::Nil => return Ok(Precondition::Any),
        Value::Table(table) => table,
        other => {
            return Err(error(format!(
                "tidings: {what}: the options must be a table, not a {}",
                other.type_name()
            )));
        }
    };
    let mut absent = false;
    let mut revision = None;
    for pair in table.pairs::<Value, Value>() {
        let (key, value) = pair?;
        match key.to_string()?.as_str() {
            "if_absent" => match value {
                Value::Boolean(b) => absent = b,
                other => {
                    return Err(error(format!(
                        "tidings: {what}: if_absent must be a boolean, not a {}",
                        other.type_name()
                    )));
                }
            },
            "if_revision" => revision = Some(value),
            other => {
                return Err(error(format!("tidings: {what}: unknown key '{other}'")));
            }
        }
    }
    match (absent, revision) {
        (true, Some(_)) => Err(error(format!(
            "tidings: {what}: give if_absent or if_revision, not both"
        ))),
        (true, None) => Ok(Precondition::Absent),
        (false, Some(revision)) => Ok(Precondition::UnchangedSince(revision_arg(&revision, what)?)),
        (false, None) => Ok(Precondition::Any),
    }
}

// -- Watching for `abort_tasks` ----------------------------------------------------------------

/// Whether a Store's tasks have been ended other than by closing it, shared by the Store and its
/// feed.
#[derive(Default)]
struct Health {
    aborted: AtomicBool,
    /// The feed has given the one Resync that says so.
    resync_given: AtomicBool,
}

impl Health {
    fn aborted(&self) -> bool {
        self.aborted.load(Ordering::SeqCst)
    }
}

/// A task beside tidings' own, on the same executor, that waits forever. If it ends without the
/// Store having been closed, `abort_tasks` ended it, and tidings' tasks with it.
struct Sentinel {
    task: AbortHandle,
    closing: Arc<AtomicBool>,
}

impl Sentinel {
    fn spawn(health: &Arc<Health>) -> Sentinel {
        let closing = Arc::new(AtomicBool::new(false));
        let watch = Watch {
            health: Arc::clone(health),
            closing: Arc::clone(&closing),
        };
        let task = tokio::spawn(async move {
            let _watch = watch;
            std::future::pending::<()>().await;
        });
        Sentinel {
            task: task.abort_handle(),
            closing,
        }
    }

    /// Ends it, as the Store is closed: not an abort.
    fn stop(&self) {
        self.closing.store(true, Ordering::SeqCst);
        self.task.abort();
    }
}

/// Held by the sentinel task, and dropped when it ends.
struct Watch {
    health: Arc<Health>,
    closing: Arc<AtomicBool>,
}

impl Drop for Watch {
    fn drop(&mut self) {
        if !self.closing.load(Ordering::SeqCst) {
            self.health.aborted.store(true, Ordering::SeqCst);
        }
    }
}

// -- Store -------------------------------------------------------------------------------------

/// A tidings Store, as Lua holds it. `None` once closed. An operation in progress holds its own
/// handle to the Store, so closing never cuts one short.
#[derive(Clone)]
struct Store(Arc<StoreState>);

struct StoreState {
    store: StdMutex<Option<tidings::Store>>,
    health: Arc<Health>,
    sentinel: Option<Sentinel>,
    /// The Backend, and the Location where there is one, for `tostring`.
    description: String,
}

impl Drop for StoreState {
    /// Lua let go of the Store without closing it: the sentinel goes with it, as tidings' tasks do.
    fn drop(&mut self) {
        if let Some(sentinel) = &self.sentinel {
            sentinel.stop();
        }
    }
}

impl Store {
    /// The Store, for an operation `what` names, unless it is closed or its tasks were aborted.
    fn handle(&self, what: &str) -> Result<tidings::Store> {
        let store = self.0.store.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(store) = store.as_ref() else {
            return Err(error(format!("tidings: {what}: the Store is closed")));
        };
        if self.0.health.aborted() {
            return Err(error(format!(
                "tidings: {what}: this Store's tasks were ended by abort_tasks, so it no longer \
                 sees Changes: close it and open it again"
            )));
        }
        Ok(store.clone())
    }

    fn close(&self) {
        let store = self
            .0
            .store
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(sentinel) = &self.0.sentinel {
            sentinel.stop();
        }
        drop(store);
    }
}

/// Registers an async method on the Store that runs `f` with a handle to it. `what` is its name as
/// a script writes it, `Store:read`, to begin its messages with.
fn store_method<A, F, Fut, R>(methods: &mut impl UserDataMethods<Store>, what: &'static str, f: F)
where
    A: mlua::FromLuaMulti + mlua::MaybeSend + 'static,
    F: Fn(Lua, tidings::Store, A, &'static str) -> Fut + mlua::MaybeSend + Sync + 'static,
    Fut: Future<Output = Result<R>> + mlua::MaybeSend + 'static,
    R: IntoLuaMulti + mlua::MaybeSend + 'static,
{
    let name = what.trim_start_matches("Store:");
    let f = Arc::new(f);
    methods.add_async_method(name, move |lua, this, args: A| {
        let handle = this.handle(what);
        let f = Arc::clone(&f);
        async move { f(lua, handle?, args, what).await }
    });
}

impl UserData for Store {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(MetaMethod::ToString, |_, this, ()| {
            let closed = this
                .0
                .store
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .is_none();
            Ok(if closed {
                format!("Store({}, closed)", this.0.description)
            } else {
                format!("Store({})", this.0.description)
            })
        });
        store_method(
            methods,
            "Store:read",
            |lua, store, path: Value, what| async move {
                let path = text_arg(&path, what, "the Path")?;
                let file = store.read(path).await.map_err(|e| failed(what, e))?;
                file.map(|file| lua.create_userdata(File(file))).transpose()
            },
        );
        store_method(
            methods,
            "Store:stat",
            |lua, store, path: Value, what| async move {
                let path = text_arg(&path, what, "the Path")?;
                let stat = store.stat(path).await.map_err(|e| failed(what, e))?;
                stat.map(|stat| {
                    let table = lua.create_table()?;
                    table.set("revision", stat.revision().to_string())?;
                    table.set("modified", Timestamp::from(stat.modified()))?;
                    Ok(table)
                })
                .transpose()
            },
        );
        store_method(
            methods,
            "Store:list",
            |_, store, prefix: Value, what| async move {
                let prefix = prefix_or_whole(&prefix, what)?;
                let paths = store.list(prefix).await.map_err(|e| failed(what, e))?;
                Ok(paths
                    .iter()
                    .map(|p| p.as_str().to_owned())
                    .collect::<Vec<_>>())
            },
        );
        store_method(
            methods,
            "Store:stat_prefix",
            |_, store, prefix: Value, what| async move {
                let prefix = prefix_or_whole(&prefix, what)?;
                let revision = store
                    .stat_prefix(prefix)
                    .await
                    .map_err(|e| failed(what, e))?;
                Ok(PrefixRevision(revision))
            },
        );
        store_method(methods, "Store:snapshot", |_, store, (), what| async move {
            let snapshot = store.snapshot().await.map_err(|e| failed(what, e))?;
            Ok(Snapshot(StdMutex::new(Some(Arc::new(snapshot)))))
        });
        methods.add_async_method("commit", |lua, this, staging: Value| async move {
            let what = "Store:commit";
            let store = this.handle(what)?;
            let staging = take_staging(&staging, what)?;
            match store.commit(staging).await {
                Ok(committed) => committed_table(&lua, &committed, false)?.into_lua_multi(&lua),
                Err(tidings::Error::Pending { committed }) => {
                    committed_table(&lua, &committed, true)?.into_lua_multi(&lua)
                }
                Err(tidings::Error::Conflict { paths }) => {
                    let conflict = lua.create_table()?;
                    let paths: Vec<_> = paths.iter().map(|p| p.as_str().to_owned()).collect();
                    conflict.set("paths", paths)?;
                    (Value::Nil, conflict).into_lua_multi(&lua)
                }
                Err(e) => Err(failed(what, e)),
            }
        });
        methods.add_method("supports_snapshots", |_, this, ()| {
            Ok(this
                .handle("Store:supports_snapshots")?
                .supports_snapshots())
        });
        methods.add_method("close", |_, this, ()| {
            this.close();
            Ok(())
        });
        methods.add_meta_method(MetaMethod::Close, |_, this, _: MultiValue| {
            this.close();
            Ok(())
        });
    }
}

/// What a Commit gave: its timestamp, the Revision of each Path it wrote, and whether it is
/// Pending.
fn committed_table(lua: &Lua, committed: &tidings::Committed, pending: bool) -> Result<Table> {
    let table = lua.create_table()?;
    table.set("timestamp", Timestamp::from(committed.timestamp()))?;
    let revisions = lua.create_table()?;
    for (path, revision) in committed.revisions() {
        revisions.set(path.as_str(), revision.to_string())?;
    }
    table.set("revisions", revisions)?;
    table.set("pending", pending)?;
    Ok(table)
}

// -- File --------------------------------------------------------------------------------------

/// A File that was read: its Path, Revision and last-modified time, and its contents.
struct File(tidings::File);

impl UserData for File {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("path", |_, this| Ok(this.0.path().as_str().to_owned()));
        fields.add_field_method_get("revision", |_, this| Ok(this.0.revision().to_string()));
        fields.add_field_method_get("modified", |_, this| Ok(Timestamp::from(this.0.modified())));
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(MetaMethod::ToString, |_, this, ()| {
            Ok(format!("File({})", this.0.path()))
        });
        methods.add_method("bytes", |lua, this, ()| lua.create_string(this.0.bytes()));
        methods.add_method("text", |_, this, ()| {
            this.0
                .text()
                .map(str::to_owned)
                .map_err(|e| failed("File:text", e))
        });
    }
}

// -- Staging -----------------------------------------------------------------------------------

/// A Staging, as Lua builds it. `None` once committed, since a Commit takes it.
struct Staging(StdMutex<Option<tidings::Staging>>);

/// Takes the Staging out of the userdata a script passed to `commit`.
fn take_staging(value: &Value, what: &str) -> Result<tidings::Staging> {
    let Value::UserData(ud) = value else {
        return Err(error(format!(
            "tidings: {what}: expected a Staging, not a {}",
            value.type_name()
        )));
    };
    let staging = ud
        .borrow::<Staging>()
        .map_err(|_| error(format!("tidings: {what}: expected a Staging")))?;
    let mut slot = staging.0.lock().unwrap_or_else(PoisonError::into_inner);
    slot.take().ok_or_else(|| {
        error(format!(
            "tidings: {what}: this Staging was already committed"
        ))
    })
}

/// Registers a method on the Staging that changes it with `f`, and gives the Staging back, so that
/// calls chain. `what` is its name as a script writes it, `Staging:write`.
fn staging_method<A>(
    methods: &mut impl UserDataMethods<Staging>,
    what: &'static str,
    f: impl Fn(&mut tidings::Staging, A, &'static str) -> Result<()> + mlua::MaybeSend + 'static,
) where
    A: mlua::FromLuaMulti,
{
    let name = what.trim_start_matches("Staging:");
    methods.add_function(name, move |_, (ud, args): (AnyUserData, A)| {
        {
            let staging = ud.borrow::<Staging>()?;
            let mut slot = staging.0.lock().unwrap_or_else(PoisonError::into_inner);
            let Some(staging) = slot.as_mut() else {
                return Err(error(format!(
                    "tidings: {what}: this Staging was already committed"
                )));
            };
            f(staging, args, what)?;
        }
        Ok(ud)
    });
}

impl UserData for Staging {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(MetaMethod::ToString, |_, this, ()| {
            let committed = this
                .0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .is_none();
            Ok(if committed {
                "Staging(committed)"
            } else {
                "Staging"
            })
        });
        staging_method(
            methods,
            "Staging:write",
            |staging, (path, contents, options): (Value, Value, Value), what| {
                let path = text_arg(&path, what, "the Path")?;
                let contents = string_arg(&contents, what, "the contents")?;
                let precondition = precondition_option(&options, what)?;
                staging
                    .write_requiring(path, contents.as_bytes().to_vec(), precondition)
                    .map_err(|e| failed(what, e))?;
                Ok(())
            },
        );
        staging_method(
            methods,
            "Staging:delete",
            |staging, (path, options): (Value, Value), what| {
                let path = text_arg(&path, what, "the Path")?;
                let precondition = precondition_option(&options, what)?;
                staging
                    .delete_requiring(path, precondition)
                    .map_err(|e| failed(what, e))?;
                Ok(())
            },
        );
        staging_method(
            methods,
            "Staging:delete_prefix",
            |staging, prefix: Value, what| {
                let prefix = text_arg(&prefix, what, "the Prefix")?;
                staging.delete_prefix(prefix).map_err(|e| failed(what, e))?;
                Ok(())
            },
        );
        staging_method(
            methods,
            "Staging:require",
            |staging, (path, condition): (Value, Value), what| {
                let path = text_arg(&path, what, "the Path")?;
                let precondition = match &condition {
                    Value::String(s) if s.as_bytes().as_ref() == b"absent" => Precondition::Absent,
                    other => Precondition::UnchangedSince(revision_arg(other, what)?),
                };
                staging
                    .require(path, precondition)
                    .map_err(|e| failed(what, e))?;
                Ok(())
            },
        );
        staging_method(
            methods,
            "Staging:require_prefix",
            |staging, (prefix, revision): (Value, Value), what| {
                let prefix = text_arg(&prefix, what, "the Prefix")?;
                let revision = match &revision {
                    Value::UserData(ud) => ud
                        .borrow::<PrefixRevision>()
                        .map(|r| r.0.clone())
                        .map_err(|_| {
                            error(format!("tidings: {what}: expected a Prefix Revision"))
                        })?,
                    other => {
                        return Err(error(format!(
                            "tidings: {what}: expected a Prefix Revision, from stat_prefix, not \
                             a {}",
                            other.type_name()
                        )));
                    }
                };
                staging
                    .require_prefix(prefix, revision)
                    .map_err(|e| failed(what, e))?;
                Ok(())
            },
        );
    }
}

// -- Prefix Revision ---------------------------------------------------------------------------

/// The state of everything under a Prefix. It prints as text, but cannot be read back from it.
struct PrefixRevision(tidings::PrefixRevision);

impl UserData for PrefixRevision {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(MetaMethod::ToString, |_, this, ()| Ok(this.0.to_string()));
        methods.add_meta_method(MetaMethod::Eq, |_, this, other: Value| {
            Ok(match &other {
                Value::UserData(ud) => ud.borrow::<PrefixRevision>().is_ok_and(|o| o.0 == this.0),
                _ => false,
            })
        });
    }
}

// -- Snapshot ----------------------------------------------------------------------------------

/// A Snapshot, as Lua holds it. `None` once closed; a read in progress holds its own handle.
struct Snapshot(StdMutex<Option<Arc<tidings::Snapshot>>>);

impl Snapshot {
    fn handle(&self, what: &str) -> Result<Arc<tidings::Snapshot>> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .ok_or_else(|| error(format!("tidings: {what}: the Snapshot is closed")))
    }

    fn close(&self) {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).take();
    }
}

impl UserData for Snapshot {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(MetaMethod::ToString, |_, this, ()| {
            let closed = this
                .0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .is_none();
            Ok(if closed {
                "Snapshot(closed)"
            } else {
                "Snapshot"
            })
        });
        methods.add_async_method("read", |lua, this, path: Value| async move {
            let what = "Snapshot:read";
            let snapshot = this.handle(what)?;
            let path = text_arg(&path, what, "the Path")?;
            let file = snapshot.read(path).await.map_err(|e| failed(what, e))?;
            file.map(|file| lua.create_userdata(File(file))).transpose()
        });
        methods.add_async_method("stat", |lua, this, path: Value| async move {
            let what = "Snapshot:stat";
            let snapshot = this.handle(what)?;
            let path = text_arg(&path, what, "the Path")?;
            let stat = snapshot.stat(path).await.map_err(|e| failed(what, e))?;
            stat.map(|stat| {
                let table = lua.create_table()?;
                table.set("revision", stat.revision().to_string())?;
                table.set("modified", Timestamp::from(stat.modified()))?;
                Ok(table)
            })
            .transpose()
        });
        methods.add_async_method("list", |_, this, prefix: Value| async move {
            let what = "Snapshot:list";
            let snapshot = this.handle(what)?;
            let prefix = prefix_or_whole(&prefix, what)?;
            let paths = snapshot.list(prefix).await.map_err(|e| failed(what, e))?;
            Ok(paths
                .iter()
                .map(|p| p.as_str().to_owned())
                .collect::<Vec<_>>())
        });
        methods.add_method("close", |_, this, ()| {
            this.close();
            Ok(())
        });
        methods.add_meta_method(MetaMethod::Close, |_, this, _: MultiValue| {
            this.close();
            Ok(())
        });
    }
}

// -- Change feed -------------------------------------------------------------------------------

/// A Store's Change feed, as Lua holds it. One task reads it at a time. `None` once closed, which
/// drops it, so that tidings stops recording Changes for it.
#[derive(Clone)]
struct Feed(Arc<FeedState>);

struct FeedState {
    feed: tokio::sync::Mutex<Option<tidings::ChangeFeed>>,
    closed: AtomicBool,
    /// Wakes a `next` that is waiting when the feed is closed.
    closing: Notify,
    health: Arc<Health>,
}

impl Feed {
    async fn next(&self, lua: &Lua) -> Result<Value> {
        let state = &self.0;
        if state.health.aborted() {
            // Changes stopped arriving when the Store's tasks were aborted: say so once, then end.
            state.feed.lock().await.take();
            if state.health.resync_given.swap(true, Ordering::SeqCst) {
                return Ok(Value::Nil);
            }
            return resync_table(lua).map(Value::Table);
        }
        // Registered before `closed` is read, so that a close in between still wakes it.
        let closing = state.closing.notified();
        tokio::pin!(closing);
        closing.as_mut().enable();
        let mut slot = state.feed.try_lock().map_err(|_| {
            error("tidings: Feed:next: the feed is already being read by another task".into())
        })?;
        if state.closed.load(Ordering::SeqCst) {
            slot.take();
            return Ok(Value::Nil);
        }
        let Some(feed) = slot.as_mut() else {
            return Ok(Value::Nil);
        };
        let item = tokio::select! {
            () = &mut closing => {
                slot.take();
                return Ok(Value::Nil);
            }
            item = feed.next() => item,
        };
        match item {
            None => Ok(Value::Nil),
            Some(FeedItem::Resync) => resync_table(lua).map(Value::Table),
            Some(FeedItem::Changes(changes)) => {
                let list = lua.create_table()?;
                for change in &changes {
                    let entry = lua.create_table()?;
                    entry.set("path", change.path.as_str())?;
                    entry.set(
                        "kind",
                        match change.kind {
                            ChangeKind::Changed => "changed",
                            ChangeKind::Removed => "removed",
                        },
                    )?;
                    entry.set(
                        "origin",
                        match change.origin {
                            Origin::Local => "local",
                            Origin::External => "external",
                        },
                    )?;
                    list.push(entry)?;
                }
                let table = lua.create_table()?;
                table.set("changes", list)?;
                Ok(Value::Table(table))
            }
        }
    }

    fn close(&self) {
        let state = &self.0;
        state.closed.store(true, Ordering::SeqCst);
        match state.feed.try_lock() {
            Ok(mut slot) => drop(slot.take()),
            // A `next` is waiting on it: it takes the feed itself when woken.
            Err(_) => state.closing.notify_waiters(),
        }
    }
}

fn resync_table(lua: &Lua) -> Result<Table> {
    let table = lua.create_table()?;
    table.set("resync", true)?;
    Ok(table)
}

impl UserData for Feed {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(MetaMethod::ToString, |_, this, ()| {
            Ok(if this.0.closed.load(Ordering::SeqCst) {
                "Feed(closed)"
            } else {
                "Feed"
            })
        });
        methods.add_async_method("next", |lua, this, ()| async move { this.next(&lua).await });
        methods.add_method("close", |_, this, ()| {
            this.close();
            Ok(())
        });
        methods.add_meta_method(MetaMethod::Close, |_, this, _: MultiValue| {
            this.close();
            Ok(())
        });
    }
}
