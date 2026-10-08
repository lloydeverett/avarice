//! `tidings` is original to avarice, not derived from Astra: ADR 0021 records why. It binds
//! [tidings](https://github.com/lloydeverett/tidings) to Lua: **tidings Stores** on the
//! filesystem, SQLite or memory, read directly, written only by committing a Staging, and reporting
//! every Change on a feed with one reader.
//!
//! The binding is thin and follows tidings' names, with these Lua-shaped differences. A Conflict
//! is returned, as `nil` and a table naming its Paths, since a script branches on it; every other
//! failure raises (ADR 0017). A Pending Commit is returned like a finished one, with `pending` set,
//! since it has happened. A Revision is a string, since it reads back from text; a Prefix Revision
//! is a userdata, since it does not. Times are `datetime`'s Timestamps. A Store, its feed and a
//! Snapshot can each be closed, where Rust would drop them. And, as in `datetime`, an argument
//! past the last one a function takes raises, rather than being dropped as Lua would drop it.
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
use std::sync::{Arc, Mutex as StdMutex, MutexGuard, PoisonError};

use mlua::{
    AnyUserData, IntoLuaMulti, Lua, LuaString, MaybeSend, MetaMethod, MultiValue, Table, UserData,
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
    error(format!("{what}: {e}"))
}

/// Locks a mutex whose holder cannot leave it half-changed, so a poisoned one is as good as any.
fn lock<T>(mutex: &StdMutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Builds the table `lua/tidings.lua` returns.
pub fn rust_half(lua: &Lua) -> Result<Value> {
    let module = lua.create_table()?;
    module.set(
        "open_fs",
        lua.create_async_function(|lua, values: MultiValue| async move {
            let what = "tidings.open_fs";
            let [location] = args(values, what)?;
            let location = location_arg(&location, what)?;
            let shown = location.display().to_string();
            let (store, feed) = tidings::Store::open_fs(location, tidings::FsOptions::default())
                .await
                .map_err(|e| failed(what, e))?;
            opened(&lua, store, feed, format!("fs, {shown}"), Tasks::Run)
        })?,
    )?;
    module.set(
        "open_sqlite",
        lua.create_async_function(|lua, values: MultiValue| async move {
            let what = "tidings.open_sqlite";
            let [location] = args(values, what)?;
            let location = location_arg(&location, what)?;
            let shown = location.display().to_string();
            let (store, feed) =
                tidings::Store::open_sqlite(location, tidings::SqliteOptions::default())
                    .await
                    .map_err(|e| failed(what, e))?;
            opened(&lua, store, feed, format!("sqlite, {shown}"), Tasks::Run)
        })?,
    )?;
    module.set(
        "open_memory",
        lua.create_function(|lua, values: MultiValue| {
            let [] = args(values, "tidings.open_memory")?;
            let (store, feed) = tidings::Store::open_memory();
            opened(lua, store, feed, "memory".to_owned(), Tasks::None)
        })?,
    )?;
    module.set(
        "detect",
        lua.create_async_function(|_, values: MultiValue| async move {
            let what = "tidings.detect";
            let [location] = args(values, what)?;
            let location = location_arg(&location, what)?;
            let kind = tidings::Store::detect(location)
                .await
                .map_err(|e| failed(what, e))?;
            Ok(kind.map(|kind| kind.to_string()))
        })?,
    )?;
    module.set(
        "staging",
        lua.create_function(|_, values: MultiValue| {
            let [] = args(values, "tidings.staging")?;
            Ok(Staging(StdMutex::new(Some(tidings::Staging::new()))))
        })?,
    )?;
    Ok(Value::Table(module))
}

/// Whether a Backend runs tasks on the executor while its Store is open: the filesystem's
/// watcher and SQLite's poller do, and the memory Backend has nothing to watch.
#[derive(Clone, Copy)]
enum Tasks {
    Run,
    None,
}

/// The Store and its feed, as `open_*` give them to Lua. A Store whose Backend runs tasks gets a
/// sentinel, to notice `abort_tasks` ending them.
fn opened(
    lua: &Lua,
    store: tidings::Store,
    feed: tidings::ChangeFeed,
    description: String,
    tasks: Tasks,
) -> Result<MultiValue> {
    let abort = Arc::new(AbortState::default());
    let sentinel = match tasks {
        Tasks::Run => Some(Sentinel::spawn(&abort)),
        Tasks::None => None,
    };
    let store = Store(Arc::new(StoreState {
        store: StdMutex::new(Some(store)),
        abort: Arc::clone(&abort),
        sentinel,
        description,
    }));
    let feed = Feed(Arc::new(FeedState {
        feed: tokio::sync::Mutex::new(Some(feed)),
        closed: AtomicBool::new(false),
        closing: Notify::new(),
        abort,
    }));
    (store, feed).into_lua_multi(lua)
}

// -- Arguments ---------------------------------------------------------------------------------

/// The `N` arguments a function takes, `nil` for those left out. Lua drops arguments past the
/// last one a function names; these raise instead, since an argument the function will not look
/// at is one the script meant something by (ADR 0017).
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

/// A string argument, refusing what Lua would coerce: a number is not a Path or contents. `name`
/// says which argument it is.
fn string_arg(value: &Value, what: &str, name: &str) -> Result<LuaString> {
    match value {
        Value::String(s) => Ok(s.clone()),
        other => Err(error(format!(
            "{what}: expected a string for {name}, got {}",
            other.type_name()
        ))),
    }
}

/// A Path or Prefix: text, since tidings' Paths are UTF-8.
fn text_arg(value: &Value, what: &str, name: &str) -> Result<String> {
    let s = string_arg(value, what, name)?;
    match s.to_str() {
        Ok(text) => Ok(text.to_owned()),
        Err(_) => Err(error(format!("{what}: {name} is not valid UTF-8"))),
    }
}

/// A Location, as the operating system is given it: its exact bytes on Unix, and elsewhere the
/// string if it is valid Unicode (ADR 0015).
fn location_arg(value: &Value, what: &str) -> Result<PathBuf> {
    let s = string_arg(value, what, "the Location")?;
    let bytes = s.as_bytes();
    if bytes.contains(&0) {
        return Err(error(format!("{what}: the Location holds a NUL byte")));
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
                "{what}: the Location is not valid Unicode, which this platform needs"
            ))),
        }
    }
}

/// A Revision, from the text a File or a Commit gave.
fn revision_arg(value: &Value, what: &str) -> Result<Revision> {
    let text = text_arg(value, what, "the Revision")?;
    text.parse()
        .map_err(|_| error(format!("{what}: '{text}' is not a Revision")))
}

/// The Prefix a reading method takes, or the whole Store for `nil`.
fn prefix_or_whole(value: &Value, what: &str) -> Result<String> {
    match value {
        Value::Nil => Ok(String::new()),
        other => text_arg(other, what, "the Prefix"),
    }
}

/// The Precondition in a write's or a delete's options table: `if_absent` or `if_revision`, or
/// none.
fn precondition_option(options: &Value, what: &str) -> Result<Precondition> {
    const KNOWN: &str = "if_absent, if_revision";
    let table = match options {
        Value::Nil => return Ok(Precondition::Any),
        Value::Table(table) => table,
        other => {
            return Err(error(format!(
                "{what}: expected a table of options, got {}",
                other.type_name()
            )));
        }
    };
    let mut absent = false;
    let mut revision = None;
    for pair in table.pairs::<Value, Value>() {
        let (key, value) = pair?;
        let Value::String(key) = key else {
            return Err(error(format!(
                "{what}: keys must be strings, got {}",
                key.type_name()
            )));
        };
        match &*key.to_string_lossy() {
            "if_absent" => match value {
                Value::Boolean(b) => absent = b,
                other => {
                    return Err(error(format!(
                        "{what}: expected a boolean for if_absent, got {}",
                        other.type_name()
                    )));
                }
            },
            "if_revision" => revision = Some(value),
            other => {
                return Err(error(format!(
                    "{what}: unknown key '{other}' (expected one of: {KNOWN})"
                )));
            }
        }
    }
    match (absent, revision) {
        (true, Some(_)) => Err(error(format!(
            "{what}: give if_absent or if_revision, not both"
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
struct AbortState {
    aborted: AtomicBool,
    /// Wakes a `next` that is waiting when the tasks are aborted.
    waking: Notify,
    /// The feed has given the one Resync that says so.
    resync_given: AtomicBool,
}

impl AbortState {
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
    fn spawn(abort: &Arc<AbortState>) -> Sentinel {
        let closing = Arc::new(AtomicBool::new(false));
        let watch = Watch {
            abort: Arc::clone(abort),
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
    abort: Arc<AbortState>,
    closing: Arc<AtomicBool>,
}

impl Drop for Watch {
    fn drop(&mut self) {
        if !self.closing.load(Ordering::SeqCst) {
            self.abort.aborted.store(true, Ordering::SeqCst);
            self.abort.waking.notify_waiters();
        }
    }
}

// -- What Store, Snapshot and Feed share -------------------------------------------------------

/// A value Lua can close, by `close` or `<close>`, and that says so when printed.
trait Closable: UserData + Sized + 'static {
    fn close(&self);
    /// What `tostring` gives.
    fn describe(&self) -> String;
}

/// Registers `close`, `__close` and `__tostring` on a [`Closable`]. `close` can be called any
/// number of times.
fn add_closing<T: Closable, M: UserDataMethods<T>>(methods: &mut M, what: &'static str) {
    methods.add_method("close", move |_, this, values: MultiValue| {
        let [] = args(values, what)?;
        this.close();
        Ok(())
    });
    methods.add_meta_method(MetaMethod::Close, |_, this, _: MultiValue| {
        this.close();
        Ok(())
    });
    methods.add_meta_method(MetaMethod::ToString, |_, this, ()| Ok(this.describe()));
}

/// A Store or a Snapshot, read the same way.
enum Source {
    Store(tidings::Store),
    Snapshot(Arc<tidings::Snapshot>),
}

impl Source {
    async fn read(&self, path: String) -> tidings::Result<Option<tidings::File>> {
        match self {
            Source::Store(store) => store.read(path).await,
            Source::Snapshot(snapshot) => snapshot.read(path).await,
        }
    }

    async fn stat(&self, path: String) -> tidings::Result<Option<tidings::Stat>> {
        match self {
            Source::Store(store) => store.stat(path).await,
            Source::Snapshot(snapshot) => snapshot.stat(path).await,
        }
    }

    async fn list(&self, prefix: String) -> tidings::Result<Vec<tidings::Path>> {
        match self {
            Source::Store(store) => store.list(prefix).await,
            Source::Snapshot(snapshot) => snapshot.list(prefix).await,
        }
    }
}

/// A userdata that reads Files: a Store or a Snapshot.
trait Readable: UserData + MaybeSend + Sync + Sized + 'static {
    /// What it reads, for an operation `what` names, unless it is closed.
    fn source(&self, what: &str) -> Result<Source>;
}

/// Registers an async method on a [`Readable`]. `what` is its name as a script writes it,
/// `Store:read`, to begin its messages with; the method's name is what follows the `:`.
fn add_reading<T, M, F, Fut, R>(methods: &mut M, what: &'static str, f: F)
where
    T: Readable,
    M: UserDataMethods<T>,
    F: Fn(Lua, Source, MultiValue, &'static str) -> Fut + MaybeSend + Sync + 'static,
    Fut: Future<Output = Result<R>> + MaybeSend + 'static,
    R: IntoLuaMulti + MaybeSend + 'static,
{
    let name = what.rsplit(':').next().unwrap_or(what);
    let f = Arc::new(f);
    methods.add_async_method(name, move |lua, this, values: MultiValue| {
        let source = this.source(what);
        let f = Arc::clone(&f);
        async move { f(lua, source?, values, what).await }
    });
}

/// Registers `read`, `stat` and `list` on a [`Readable`], under the names a script writes them by.
fn add_reads<T: Readable, M: UserDataMethods<T>>(
    methods: &mut M,
    [read, stat, list]: [&'static str; 3],
) {
    add_reading(methods, read, |lua, source, values, what| async move {
        let [path] = args(values, what)?;
        let path = text_arg(&path, what, "the Path")?;
        let file = source.read(path).await.map_err(|e| failed(what, e))?;
        file.map(|file| lua.create_userdata(File(file))).transpose()
    });
    add_reading(methods, stat, |lua, source, values, what| async move {
        let [path] = args(values, what)?;
        let path = text_arg(&path, what, "the Path")?;
        let stat = source.stat(path).await.map_err(|e| failed(what, e))?;
        stat.map(|stat| {
            let table = lua.create_table()?;
            table.set("revision", stat.revision().to_string())?;
            table.set("modified", Timestamp::from(stat.modified()))?;
            Ok(table)
        })
        .transpose()
    });
    add_reading(methods, list, |_, source, values, what| async move {
        let [prefix] = args(values, what)?;
        let prefix = prefix_or_whole(&prefix, what)?;
        let paths = source.list(prefix).await.map_err(|e| failed(what, e))?;
        Ok(paths
            .iter()
            .map(|p| p.as_str().to_owned())
            .collect::<Vec<_>>())
    });
}

// -- Store -------------------------------------------------------------------------------------

/// A tidings Store, as Lua holds it. `None` once closed. An operation in progress holds its own
/// handle to the Store, so closing never cuts one short.
#[derive(Clone)]
struct Store(Arc<StoreState>);

struct StoreState {
    store: StdMutex<Option<tidings::Store>>,
    abort: Arc<AbortState>,
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
        let store = lock(&self.0.store);
        let Some(store) = store.as_ref() else {
            return Err(error(format!("{what}: the Store is closed")));
        };
        if self.0.abort.aborted() {
            return Err(error(format!(
                "{what}: this Store's tasks were ended by abort_tasks, so it no longer sees \
                 Changes: close it and open it again"
            )));
        }
        Ok(store.clone())
    }
}

impl Closable for Store {
    fn close(&self) {
        let store = lock(&self.0.store).take();
        if let Some(sentinel) = &self.0.sentinel {
            sentinel.stop();
        }
        drop(store);
    }

    fn describe(&self) -> String {
        if lock(&self.0.store).is_none() {
            format!("Store({}, closed)", self.0.description)
        } else {
            format!("Store({})", self.0.description)
        }
    }
}

impl Readable for Store {
    fn source(&self, what: &str) -> Result<Source> {
        self.handle(what).map(Source::Store)
    }
}

impl UserData for Store {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        add_closing(methods, "Store:close");
        add_reads(methods, ["Store:read", "Store:stat", "Store:list"]);
        methods.add_async_method("stat_prefix", |_, this, values: MultiValue| async move {
            let what = "Store:stat_prefix";
            let [prefix] = args(values, what)?;
            let store = this.handle(what)?;
            let prefix = prefix_or_whole(&prefix, what)?;
            let revision = store
                .stat_prefix(prefix)
                .await
                .map_err(|e| failed(what, e))?;
            Ok(PrefixRevision(revision))
        });
        methods.add_async_method("snapshot", |_, this, values: MultiValue| async move {
            let what = "Store:snapshot";
            let [] = args(values, what)?;
            let store = this.handle(what)?;
            let snapshot = store.snapshot().await.map_err(|e| failed(what, e))?;
            Ok(Snapshot(StdMutex::new(Some(Arc::new(snapshot)))))
        });
        methods.add_async_method("commit", |lua, this, values: MultiValue| async move {
            let what = "Store:commit";
            let [staging] = args(values, what)?;
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
        methods.add_method("supports_snapshots", |_, this, values: MultiValue| {
            let what = "Store:supports_snapshots";
            let [] = args(values, what)?;
            Ok(this.handle(what)?.supports_snapshots())
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
        methods.add_method("bytes", |lua, this, values: MultiValue| {
            let [] = args(values, "File:bytes")?;
            lua.create_string(this.0.bytes())
        });
        methods.add_method("text", |_, this, values: MultiValue| {
            let what = "File:text";
            let [] = args(values, what)?;
            this.0
                .text()
                .map(str::to_owned)
                .map_err(|e| failed(what, e))
        });
    }
}

// -- Staging -----------------------------------------------------------------------------------

/// A Staging, as Lua builds it. `None` once committed, since a Commit takes it.
struct Staging(StdMutex<Option<tidings::Staging>>);

fn already_committed(what: &str) -> mlua::Error {
    error(format!("{what}: this Staging was already committed"))
}

/// Takes the Staging out of the userdata a script passed to `commit`.
fn take_staging(value: &Value, what: &str) -> Result<tidings::Staging> {
    let staging = match value {
        Value::UserData(ud) => ud.borrow::<Staging>().ok(),
        _ => None,
    };
    let Some(staging) = staging else {
        return Err(error(format!(
            "{what}: expected a Staging, got {}",
            value.type_name()
        )));
    };
    lock(&staging.0)
        .take()
        .ok_or_else(|| already_committed(what))
}

/// Registers a method on the Staging that changes it with `f`, and gives the Staging back, so that
/// calls chain. `what` is its name as a script writes it, `Staging:write`.
fn add_staging<const N: usize>(
    methods: &mut impl UserDataMethods<Staging>,
    what: &'static str,
    f: impl Fn(&mut tidings::Staging, [Value; N], &'static str) -> Result<()> + MaybeSend + 'static,
) {
    let name = what.trim_start_matches("Staging:");
    methods.add_function(name, move |_, (ud, values): (AnyUserData, MultiValue)| {
        let values = args(values, what)?;
        {
            let staging = ud.borrow::<Staging>()?;
            let mut slot = lock(&staging.0);
            let staging = slot.as_mut().ok_or_else(|| already_committed(what))?;
            f(staging, values, what)?;
        }
        Ok(ud)
    });
}

impl UserData for Staging {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(MetaMethod::ToString, |_, this, ()| {
            Ok(if lock(&this.0).is_none() {
                "Staging(committed)"
            } else {
                "Staging"
            })
        });
        add_staging(
            methods,
            "Staging:write",
            |staging, [path, contents, options], what| {
                let path = text_arg(&path, what, "the Path")?;
                let contents = string_arg(&contents, what, "the contents")?;
                let precondition = precondition_option(&options, what)?;
                staging
                    .write_requiring(path, contents.as_bytes().to_vec(), precondition)
                    .map_err(|e| failed(what, e))?;
                Ok(())
            },
        );
        add_staging(
            methods,
            "Staging:delete",
            |staging, [path, options], what| {
                let path = text_arg(&path, what, "the Path")?;
                let precondition = precondition_option(&options, what)?;
                staging
                    .delete_requiring(path, precondition)
                    .map_err(|e| failed(what, e))?;
                Ok(())
            },
        );
        add_staging(
            methods,
            "Staging:delete_prefix",
            |staging, [prefix], what| {
                let prefix = text_arg(&prefix, what, "the Prefix")?;
                staging.delete_prefix(prefix).map_err(|e| failed(what, e))?;
                Ok(())
            },
        );
        add_staging(
            methods,
            "Staging:require",
            |staging, [path, condition], what| {
                let path = text_arg(&path, what, "the Path")?;
                // A Revision is 32 hexadecimal digits, so it is never "absent".
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
        add_staging(
            methods,
            "Staging:require_prefix",
            |staging, [prefix, revision], what| {
                let prefix = text_arg(&prefix, what, "the Prefix")?;
                let given = match &revision {
                    Value::UserData(ud) => ud.borrow::<PrefixRevision>().ok().map(|r| r.0.clone()),
                    _ => None,
                };
                let Some(given) = given else {
                    return Err(error(format!(
                        "{what}: expected a Prefix Revision, from stat_prefix, got {}",
                        revision.type_name()
                    )));
                };
                staging
                    .require_prefix(prefix, given)
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

impl Closable for Snapshot {
    fn close(&self) {
        lock(&self.0).take();
    }

    fn describe(&self) -> String {
        if lock(&self.0).is_none() {
            "Snapshot(closed)".to_owned()
        } else {
            "Snapshot".to_owned()
        }
    }
}

impl Readable for Snapshot {
    fn source(&self, what: &str) -> Result<Source> {
        lock(&self.0)
            .clone()
            .map(Source::Snapshot)
            .ok_or_else(|| error(format!("{what}: the Snapshot is closed")))
    }
}

impl UserData for Snapshot {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        add_closing(methods, "Snapshot:close");
        add_reads(methods, ["Snapshot:read", "Snapshot:stat", "Snapshot:list"]);
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
    abort: Arc<AbortState>,
}

impl Feed {
    async fn next(&self, lua: &Lua) -> Result<Value> {
        let state = &self.0;
        // Both registered before `closed` and `aborted` are read, so that a close or an abort in
        // between still wakes it.
        let closing = state.closing.notified();
        let aborting = state.abort.waking.notified();
        tokio::pin!(closing, aborting);
        closing.as_mut().enable();
        aborting.as_mut().enable();
        let mut slot = state.feed.try_lock().map_err(|_| {
            error("Feed:next: the feed is already being read by another task".into())
        })?;
        if state.abort.aborted() {
            slot.take();
            return self.resync_once(lua);
        }
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
            () = &mut aborting => {
                slot.take();
                return self.resync_once(lua);
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

    /// Changes stopped arriving when the Store's tasks were aborted: a Resync says so, once, and
    /// then the feed has ended.
    fn resync_once(&self, lua: &Lua) -> Result<Value> {
        if self.0.abort.resync_given.swap(true, Ordering::SeqCst) {
            Ok(Value::Nil)
        } else {
            resync_table(lua).map(Value::Table)
        }
    }
}

impl Closable for Feed {
    fn close(&self) {
        let state = &self.0;
        state.closed.store(true, Ordering::SeqCst);
        match state.feed.try_lock() {
            Ok(mut slot) => drop(slot.take()),
            // A `next` is waiting on it: it takes the feed itself when woken.
            Err(_) => state.closing.notify_waiters(),
        }
    }

    fn describe(&self) -> String {
        if self.0.closed.load(Ordering::SeqCst) {
            "Feed(closed)".to_owned()
        } else {
            "Feed".to_owned()
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
        add_closing(methods, "Feed:close");
        methods.add_async_method("next", |lua, this, values: MultiValue| async move {
            let [] = args(values, "Feed:next")?;
            this.next(&lua).await
        });
    }
}
