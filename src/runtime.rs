//! The runtime, and the builder that configures one.

use std::collections::HashMap;
use std::future::Future;
use std::io::Write;
use std::sync::{Arc, Mutex, PoisonError, RwLock, TryLockError};
use std::time::Duration;

use mlua::chunk::{AsChunk, Chunk, ChunkMode};
use mlua::{FromLuaMulti, IntoLua, Lua, LuaOptions, StdLib, Table, Value};

use crate::ansi;
use crate::error::{Error, Result};
use crate::limits::{
    self, CancelHandle, DEFAULT_CHECK_INTERVAL, Execution, Limits, cancelled_error,
    unless_cancelled,
};
use crate::lock::lock;
use crate::module::{ModuleName, ModuleStore};
use crate::print::{self, Sink};
use crate::profile::Profile;
use crate::stdlib::StdModules;

/// The registry key Lua itself uses for its loaded-module table.
const LOADED: &str = "_LOADED";

/// How often [`Runtime::wait_for_tasks`] looks at the executor again.
///
/// The executor cannot say when its last task finishes, so the wait asks. This is how late a
/// program that has just run out of work can be to notice it.
const TASK_POLL: Duration = Duration::from_millis(5);

// `Send + Sync` because mlua's `send` feature, which the stdlib needs for its tasks, makes
// the `require` function Lua holds `Send`, and `require` reaches the loaders.
type Loader = Arc<dyn Fn(&Lua) -> mlua::Result<Value> + Send + Sync>;

/// What `require` resolves against, mutable after the runtime is built.
#[derive(Default)]
struct Modules {
    store: Mutex<Option<Arc<dyn ModuleStore>>>,
    loaders: Mutex<HashMap<ModuleName, Loader>>,
    loading: Mutex<Vec<ModuleName>>,
}

/// A Lua runtime: a Lua state, the libraries a [`Profile`] granted it, the limits it runs under,
/// and the executor that drives it.
///
/// There is one way to run a chunk and it is asynchronous, because a stdlib module may await
/// something — an HTTP response, a timer — while Lua waits for it. [`exec`](Self::exec) and
/// [`eval`](Self::eval) return futures. The runtime owns a current-thread tokio runtime, and
/// [`block_on`](Self::block_on) drives a future on it for a caller who is not itself async.
///
/// ```
/// use avarice_rt::{Profile, Runtime};
///
/// let rt = Runtime::new(Profile::Sandbox)?;
/// let sum: i64 = rt.block_on(rt.eval("1 + 2", "=example"))?;
/// assert_eq!(sum, 3);
/// # Ok::<_, avarice_rt::Error>(())
/// ```
///
/// Calling `block_on` from inside another tokio runtime panics, with tokio's own message. An
/// embedder that is already async should build the `Runtime` on a thread of its own and talk to
/// that thread, since one Lua state is not reentrant in any case.
///
/// # Tasks
///
/// Lua code can leave work running: the stdlib's `utils.spawn_task` and friends put a task on the
/// executor and hand Lua a handle to it. Tasks run only while the executor is being driven, that
/// is, during a call to [`block_on`](Self::block_on), and nothing ends them when the chunk that
/// spawned them does. A host decides what should become of them:
///
/// - [`outstanding_tasks`](Self::outstanding_tasks) says how many there are;
/// - [`wait_for_tasks`](Self::wait_for_tasks) drives them until there are none;
/// - [`abort_tasks`](Self::abort_tasks) ends them all;
/// - dropping the runtime drops them, without waiting.
///
/// The runtime keeps no list of them. Their handles live inside Lua, so what it can report is
/// what the executor knows: every task on it, whoever spawned it.
pub struct Runtime {
    // Declared first so that it drops first: outstanding tasks hold Lua values, and should be gone
    // before the state they point into. Behind a lock because aborting the tasks means replacing
    // it, since a tokio runtime cannot be told to drop only its tasks.
    executor: RwLock<tokio::runtime::Runtime>,
    lua: Lua,
    limits: Arc<Limits>,
    modules: Arc<Modules>,
    sink: Sink,
    profile: Profile,
    binary_chunks: bool,
    memory_limit: Option<usize>,
}

impl Runtime {
    /// Builds a runtime from a profile's defaults.
    pub fn new(profile: Profile) -> Result<Self> {
        Runtime::builder(profile).build()
    }

    /// Starts configuring a runtime, beginning from a profile's defaults.
    pub fn builder(profile: Profile) -> RuntimeBuilder {
        RuntimeBuilder::new(profile)
    }

    /// The profile this runtime was built from.
    ///
    /// Describes where the runtime started, not where it is now: the builder may have overridden
    /// anything the profile set, and host modules may have been registered since.
    pub fn profile(&self) -> Profile {
        self.profile
    }

    /// Drives `future` to completion on this runtime's executor, blocking the calling thread.
    ///
    /// Tasks that Lua code spawns run whenever the executor does, which is only while a call to
    /// this is in progress; they are not driven between calls, and they are not waited for when
    /// `future` finishes. See [`wait_for_tasks`](Self::wait_for_tasks).
    ///
    /// # Panics
    ///
    /// If called from within another tokio runtime. See the type's documentation.
    pub fn block_on<F: Future>(&self, future: F) -> F::Output {
        self.executor().block_on(future)
    }

    fn executor(&self) -> std::sync::RwLockReadGuard<'_, tokio::runtime::Runtime> {
        self.executor.read().unwrap_or_else(PoisonError::into_inner)
    }

    /// How many tasks are on the executor and have not finished.
    ///
    /// Counts every task, whoever spawned it: those Lua spawned, and any a stdlib module spawns
    /// for itself. A task that is waiting, on a timer or on the network, is outstanding.
    pub fn outstanding_tasks(&self) -> usize {
        self.executor().handle().metrics().num_alive_tasks()
    }

    /// Drives the executor until no task is outstanding, including those that tasks spawn on the
    /// way.
    ///
    /// The returned future must be driven by this runtime's executor: pass it to
    /// [`block_on`](Self::block_on). This is how a program that spawned tasks and then ran out of
    /// things to do finishes them, since nothing else drives them once the chunk is over.
    ///
    /// The wait is a top-level execution of its own, under the same limits as any other: a time
    /// limit gives it a fresh budget, and a [`CancelHandle`] that trips ends it with
    /// [`Cancelled`](crate::Cancelled). Either leaves the tasks that were still running as they
    /// were; [`abort_tasks`](Self::abort_tasks) is how to be rid of them.
    ///
    /// A time limit is enforced where Lua runs, as for any execution, and does not cut the wait
    /// itself short: a task idling on a timer is waited for past the deadline. Once a task's Lua
    /// runs and the hook finds the budget spent, the wait gives up, and fails as if it had been
    /// the one stopped, even though the stdlib swallows the task's own error. So a task that
    /// never finishes, an interval for one, keeps this waiting until its next run past the
    /// deadline, or a cancel. That is the meaning of waiting for it.
    pub async fn wait_for_tasks(&self) -> Result<()> {
        self.run(async {
            // What the hook has latched, not the clock: the limit ends the wait when Lua finds it
            // spent, not when it runs out.
            while self.outstanding_tasks() > 0 {
                if let Some(err) = self.limits.stopped() {
                    return Err(err);
                }
                tokio::time::sleep(TASK_POLL).await;
            }
            self.limits.stopped().map_or(Ok(()), Err)
        })
        .await
    }

    /// Ends every outstanding task, and says how many there were.
    ///
    /// The tasks are dropped where they stand: a task in the middle of a call to Lua does not see
    /// an error, it simply never resumes. The Lua state is untouched, so globals, loaded modules
    /// and anything the tasks had already done remain, and the runtime carries on with a fresh
    /// executor.
    ///
    /// Work a task had handed to tokio's blocking pool, a file read for one, is not stopped: it
    /// finishes unobserved, and nothing waits for it.
    ///
    /// # Panics
    ///
    /// If the executor is running, that is, from inside [`block_on`](Self::block_on) or from
    /// another thread while a call to it is in progress. Call it once that has returned.
    pub fn abort_tasks(&self) -> Result<usize> {
        let fresh = new_executor()?;
        let old = {
            let mut slot = match self.executor.try_write() {
                Ok(slot) => slot,
                Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
                Err(TryLockError::WouldBlock) => panic!(
                    "abort_tasks was called while the executor is running: call it after \
                     `block_on` has returned"
                ),
            };
            std::mem::replace(&mut *slot, fresh)
        };
        let aborted = old.handle().metrics().num_alive_tasks();
        // Not a plain drop, which would wait for blocking work the tasks had started.
        old.shutdown_background();
        Ok(aborted)
    }

    /// The underlying Lua state, for everything this API does not wrap.
    ///
    /// Two things to know when driving Lua directly through this handle. Chunks loaded with
    /// [`Lua::load`] do not inherit this runtime's refusal of binary chunks — use
    /// [`Runtime::load`] for that. And a time limit is armed only while an
    /// [`Execution`] guard is held, so run calls made from here through [`Runtime::run`], or
    /// take a guard from [`Runtime::enter`]; cancellation needs no arming and applies to Lua that
    /// is running throughout, though only `run` also stops a call that is waiting.
    pub fn lua(&self) -> &Lua {
        &self.lua
    }

    /// A handle for cancelling this runtime from another thread, if one was configured.
    pub fn cancel_handle(&self) -> Option<CancelHandle> {
        self.limits.cancel_handle().cloned()
    }

    /// The wall-clock limit each top-level execution runs under, if any.
    pub fn time_limit(&self) -> Option<Duration> {
        self.limits.time_limit()
    }

    /// Marks the start of a top-level execution, arming the time limit until the guard drops.
    ///
    /// [`exec`](Self::exec), [`eval`](Self::eval) and [`run`](Self::run) do this for themselves;
    /// call it when driving Lua through [`Runtime::lua`] in a way `run` cannot express. Nesting is
    /// safe: an inner guard does not extend the outer execution's budget.
    ///
    /// The clock keeps running while the execution awaits, so time spent waiting on a slow
    /// response counts against the budget.
    ///
    /// If the execution was stopped, by a cancel or a limit, dropping the outermost guard frees
    /// what a future dropped part way was waiting on, such as a Child: drop the future first.
    ///
    /// Fails immediately if the runtime's [`CancelHandle`] is already tripped, so that a chunk
    /// too short to reach a single hook tick cannot slip past a cancel.
    pub fn enter(&self) -> Result<Execution> {
        if let Some(err) = self.limits.precheck() {
            return Err(err.into());
        }
        Ok(Execution::new(Arc::clone(&self.limits), &self.lua))
    }

    /// Prepares a chunk, applying this runtime's chunk rules.
    ///
    /// `name` is the chunk name Lua reports in errors and tracebacks. By Lua's convention a
    /// leading `@` means "this is a file path" and a leading `=` means "use this verbatim";
    /// anything else is shown quoted as a source string.
    pub fn load<'a>(&self, source: impl AsChunk + 'a, name: impl Into<String>) -> Chunk<'a> {
        let chunk = self.lua.load(source).set_name(name);
        enforce_chunk_mode(chunk, self.binary_chunks)
    }

    /// Runs `future`, which drives Lua, as one top-level execution.
    ///
    /// This is what [`exec`](Self::exec) and [`eval`](Self::eval) are made of, for when a chunk
    /// needs calling with arguments, or a call needs several steps under one time budget: it takes
    /// an [`Execution`] as [`enter`](Self::enter) does, and it also gives way if the runtime's
    /// [`CancelHandle`] trips while `future` is waiting on something, dropping `future` and failing
    /// with [`Cancelled`](crate::Cancelled). Lua that is running is stopped by the limit hook, as
    /// ever; this is for the time when it is not.
    ///
    /// The returned future must be driven by this runtime's executor: pass it to
    /// [`block_on`](Self::block_on).
    pub async fn run<T>(&self, future: impl Future<Output = mlua::Result<T>>) -> Result<T> {
        let _execution = self.enter()?;
        let outcome = unless_cancelled(self.limits.cancel_handle(), future).await;
        match outcome {
            Some(result) => Ok(result?),
            None => Err(cancelled_error().into()),
        }
    }

    /// Runs a chunk for its side effects.
    ///
    /// The returned future must be driven by this runtime's executor: pass it to
    /// [`block_on`](Self::block_on).
    pub async fn exec(&self, source: impl AsChunk, name: impl Into<String>) -> Result<()> {
        self.run(self.load(source, name).exec_async()).await
    }

    /// Runs a chunk and converts its result.
    ///
    /// A chunk that parses as an expression is evaluated as one, so `eval::<i64>("1 + 2")`
    /// works as well as `eval::<()>("x = 1")`.
    ///
    /// The returned future must be driven by this runtime's executor: pass it to
    /// [`block_on`](Self::block_on).
    pub async fn eval<R: FromLuaMulti>(
        &self,
        source: impl AsChunk,
        name: impl Into<String>,
    ) -> Result<R> {
        self.run(self.load(source, name).eval_async()).await
    }

    /// Makes `value` the module `name`, resolvable by `require` from then on.
    ///
    /// This is how capability reaches Lua. Which modules a runtime has is decided here in Rust;
    /// Lua code never causes one to load.
    pub fn register_module(&self, name: &str, value: impl IntoLua) -> Result<()> {
        let name = ModuleName::new(name)?;
        self.lua.register_module(name.as_str(), value)?;
        Ok(())
    }

    /// Registers a module built on first `require`, and cached thereafter.
    ///
    /// Use this when building the module is expensive, or when it need not happen at all if the
    /// program never asks for it. The loader runs inside the Lua state, under the same limits as
    /// the code that called `require`.
    ///
    /// The loader must be `Send + Sync`: Lua's `require` holds it, and the crate turns on
    /// mlua's `send` feature (the stdlib's tasks need it), which makes everything Lua holds `Send`.
    pub fn register_lazy_module<F>(&self, name: &str, loader: F) -> Result<()>
    where
        F: Fn(&Lua) -> mlua::Result<Value> + Send + Sync + 'static,
    {
        let name = ModuleName::new(name)?;
        lock(&self.modules.loaders).insert(name, Arc::new(loader));
        Ok(())
    }

    /// Whether a module of this name is registered or already loaded.
    ///
    /// Says nothing about whether the module store could supply it; that is only known by
    /// asking the store.
    pub fn has_module(&self, name: &str) -> bool {
        let Ok(name) = ModuleName::new(name) else {
            return false;
        };
        if lock(&self.modules.loaders).contains_key(&name) {
            return true;
        }
        match self.lua.named_registry_value::<Table>(LOADED) {
            Ok(loaded) => loaded
                .raw_get::<Value>(name.as_str())
                .is_ok_and(|v| !v.is_nil()),
            Err(_) => false,
        }
    }

    /// Redirects `print` to `writer`, from now on.
    ///
    /// The **write sink** is where `print` sends what it prints; until it is replaced it is the
    /// process's standard output. Each `print` call writes its whole line and then flushes, so
    /// a sink that buffers sees no data held back.
    pub fn set_write_sink(&self, writer: impl Write + Send + 'static) {
        *lock(&self.sink) = Box::new(writer);
    }

    /// Replaces the module store `require` falls back to.
    pub fn set_store(&self, store: impl ModuleStore) {
        *lock(&self.modules.store) = Some(Arc::new(store));
    }

    /// Removes the module store, leaving only host-registered modules resolvable.
    pub fn clear_store(&self) {
        *lock(&self.modules.store) = None;
    }

    /// The bytes of Lua-visible memory currently in use.
    pub fn used_memory(&self) -> usize {
        self.lua.used_memory()
    }

    /// The memory ceiling in force, or `None` if memory is unlimited.
    pub fn memory_limit(&self) -> Option<usize> {
        self.memory_limit
    }
}

impl std::fmt::Debug for Runtime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Runtime")
            .field("profile", &self.profile)
            .field("memory_limit", &self.memory_limit())
            .field("time_limit", &self.time_limit())
            .finish_non_exhaustive()
    }
}

/// Configures a [`Runtime`], starting from a [`Profile`]'s defaults.
#[derive(Clone)]
pub struct RuntimeBuilder {
    profile: Profile,
    std_libs: StdLib,
    std_modules: StdModules,
    memory_limit: Option<usize>,
    time_limit: Option<Duration>,
    cancel: Option<CancelHandle>,
    check_interval: u32,
    binary_chunks: bool,
    store: Option<Arc<dyn ModuleStore>>,
    sink: Option<Sink>,
}

impl std::fmt::Debug for RuntimeBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeBuilder")
            .field("profile", &self.profile)
            .field("memory_limit", &self.memory_limit)
            .field("time_limit", &self.time_limit)
            .field("binary_chunks", &self.binary_chunks)
            .field("store", &self.store.as_ref().map(|s| s.describe()))
            .finish_non_exhaustive()
    }
}

impl RuntimeBuilder {
    /// Starts from a profile's defaults.
    pub fn new(profile: Profile) -> Self {
        RuntimeBuilder {
            profile,
            std_libs: profile.std_libs(),
            std_modules: profile.std_modules(),
            memory_limit: profile.memory_limit(),
            time_limit: None,
            cancel: None,
            check_interval: DEFAULT_CHECK_INTERVAL,
            binary_chunks: profile.allows_binary_chunks(),
            store: None,
            sink: None,
        }
    }

    /// Replaces the set of standard libraries to open.
    ///
    /// [`StdLib::PACKAGE`] and [`StdLib::DEBUG`] are not accepted: [`build`](Self::build)
    /// refuses them rather than dropping them quietly. That also rules out
    /// [`StdLib::ALL_SAFE`], which for Lua — as opposed to Luau — includes `package`.
    pub fn std_libs(mut self, libs: StdLib) -> Self {
        self.std_libs = libs;
        self
    }

    /// Opens these libraries in addition to the profile's.
    ///
    /// Subject to the same refusals as [`std_libs`](Self::std_libs).
    pub fn with_std_libs(mut self, libs: StdLib) -> Self {
        self.std_libs |= libs;
        self
    }

    /// Withholds these libraries from the profile's set.
    pub fn without_std_libs(mut self, libs: StdLib) -> Self {
        self.std_libs &= !libs;
        self
    }

    /// Replaces the set of stdlib modules the runtime registers.
    ///
    /// A profile is a set of defaults, so this can add a module to a sandbox as readily as it
    /// can take one from trusted mode. Nothing here is refused the way [`StdLib::PACKAGE`] is:
    /// a stdlib module carries no privilege an embedder's own host module lacks.
    ///
    /// A module that is not compiled in (see [`StdModules::ALL`]) cannot be registered, and
    /// [`build`](Self::build) returns an error naming the Cargo feature it needs.
    pub fn std_modules(mut self, modules: StdModules) -> Self {
        self.std_modules = modules;
        self
    }

    /// Adds stdlib modules to the set the runtime registers.
    ///
    /// Adding one that is not compiled in is an error at [`build`](Self::build), as for
    /// [`std_modules`](Self::std_modules).
    pub fn with_std_modules(mut self, modules: StdModules) -> Self {
        self.std_modules |= modules;
        self
    }

    /// Removes stdlib modules from the set the runtime registers.
    ///
    /// Removing one that is not compiled in does nothing, since it asks for less.
    pub fn without_std_modules(mut self, modules: StdModules) -> Self {
        self.std_modules &= !modules;
        self
    }

    /// Caps the memory Lua may allocate.
    ///
    /// Enforced by the allocator, so it covers everything Lua allocates rather than only what
    /// the GC can see. Allocation past the cap raises a Lua error that scripts can catch, so
    /// this bounds memory use; it does not guarantee the script stops.
    pub fn memory_limit(mut self, bytes: usize) -> Self {
        self.memory_limit = Some(bytes);
        self
    }

    /// Lets Lua allocate without a ceiling.
    pub fn unlimited_memory(mut self) -> Self {
        self.memory_limit = None;
        self
    }

    /// Limits how long one top-level execution may run for.
    ///
    /// The clock starts when the future [`Runtime::exec`] or [`Runtime::eval`] returns is first
    /// polled, or when [`Runtime::enter`] is called, and stops when that finishes, so the limit is
    /// per execution rather than for the runtime's lifetime. Installs the limit hook, which costs a
    /// little throughput.
    ///
    /// The hook is what enforces it, so it stops Lua that is running. The clock keeps running
    /// while the execution awaits, on a response or a Child, but the await is not interrupted: the
    /// limit stops the execution when it next runs Lua, which may be never. A
    /// [`CancelHandle`](crate::CancelHandle) is what ends a wait (ADR 0004).
    /// [`Runtime::wait_for_tasks`] is no exception: it gives up once a task's Lua is stopped.
    pub fn time_limit(mut self, limit: Duration) -> Self {
        self.time_limit = Some(limit);
        self
    }

    /// Lets executions run for as long as they like.
    pub fn no_time_limit(mut self) -> Self {
        self.time_limit = None;
        self
    }

    /// Makes this runtime cancellable through `handle`.
    ///
    /// Pass a handle you have cloned elsewhere — into a Ctrl-C handler, say — or take the
    /// runtime's own with [`Runtime::cancel_handle`] afterwards.
    pub fn cancel_handle(mut self, handle: CancelHandle) -> Self {
        self.cancel = Some(handle);
        self
    }

    /// How many VM instructions pass between limit checks.
    ///
    /// Lower reacts sooner, higher costs less. Ignored when there is nothing to enforce.
    pub fn check_interval(mut self, instructions: u32) -> Self {
        self.check_interval = instructions.max(1);
        self
    }

    /// Whether Lua may load precompiled chunks.
    ///
    /// Leave this off for untrusted code. Lua does not verify bytecode, so a handcrafted chunk
    /// is as good as arbitrary code in the host process; the sandbox profile refuses binary
    /// chunks for exactly that reason.
    pub fn allow_binary_chunks(mut self, allow: bool) -> Self {
        self.binary_chunks = allow;
        self
    }

    /// Sets the module store `require` falls back to when no host module matches.
    pub fn store(mut self, store: impl ModuleStore) -> Self {
        self.store = Some(Arc::new(store));
        self
    }

    /// Sends what `print` prints to `writer`, instead of to standard output.
    ///
    /// Clones of a builder share the sink, so two runtimes built from one write to the same
    /// place. See [`Runtime::set_write_sink`].
    pub fn write_sink(mut self, writer: impl Write + Send + 'static) -> Self {
        self.sink = Some(print::new_sink(writer));
        self
    }

    /// Builds the runtime.
    pub fn build(self) -> Result<Runtime> {
        // Two libraries are refused outright rather than quietly dropped, for two different
        // reasons.
        //
        // `debug` mlua will not open on a safe state at all, because parts of it can violate
        // the invariants that safety rests on — and the unsafe constructor is not a trade worth
        // making, since it exists to let *Lua* load C modules.
        //
        // `package` mlua would open quite happily; the refusal is ours. `require` is this
        // runtime's own, and `package.loadlib` plus a searcher list that reaches a `.so` is
        // exactly the capability the runtime exists to remove. `StdLib::ALL_SAFE` contains it,
        // which is the likeliest way it would arrive by accident.
        if self.std_libs.contains(StdLib::DEBUG) {
            return Err(Error::Config(
                "the debug library cannot be opened: parts of it can violate mlua's safety \
                 invariants, so a `debug` table carrying only `traceback` is installed instead"
                    .to_string(),
            ));
        }
        if self.std_libs.contains(StdLib::PACKAGE) {
            return Err(Error::Config(
                "the package library cannot be opened: `require` is this runtime's own, and \
                 `package.loadlib` would hand Lua back the C module loading the runtime exists \
                 to take away. Register the module from Rust instead"
                    .to_string(),
            ));
        }
        // `print` and the core `ansi` module are Lua (ADR 0005, ADR 0011), and use these two. A
        // runtime without one is refused rather than given a `print` that quietly does less.
        for (lib, name) in [(StdLib::STRING, "string"), (StdLib::TABLE, "table")] {
            if !self.std_libs.contains(lib) {
                return Err(Error::Config(format!(
                    "the {name} library is required: `print` and the core `ansi` module are \
                     written in Lua and use it"
                )));
            }
        }
        // Refused rather than dropped, as `package` is: a runtime that quietly lacks a module it
        // was asked for disagrees with the code that asked (ADR 0007).
        self.std_modules
            .require_compiled_in()
            .map_err(|e| Error::Config(e.to_string()))?;
        let lua = Lua::new_with(self.std_libs, LuaOptions::default())?;

        let modules = Arc::new(Modules {
            store: Mutex::new(self.store),
            ..Modules::default()
        });
        let limits = Arc::new(Limits::new(self.cancel, self.time_limit));

        install_require(&lua, Arc::clone(&modules), self.binary_chunks)?;
        install_traceback(&lua)?;
        restrict_base_library(&lua, self.binary_chunks)?;
        let sink = self.sink.unwrap_or_else(print::default_sink);
        print::install(&lua, Arc::clone(&sink))?;
        install_stdlib_list(&lua, self.std_modules)?;

        // Last, so that none of our own setup can trip the limits we install.
        if let Some(bytes) = self.memory_limit {
            lua.set_memory_limit(bytes)?;
        }
        if limits.needs_hook() {
            limits::install_hook(&lua, Arc::clone(&limits), self.check_interval)?;
        }

        let executor = RwLock::new(new_executor()?);

        let runtime = Runtime {
            executor,
            lua,
            limits,
            modules,
            sink,
            profile: self.profile,
            binary_chunks: self.binary_chunks,
            memory_limit: self.memory_limit,
        };

        // `ansi` is a core module: registered in every runtime, whatever the profile, and by the
        // path an embedder's own lazy module takes, so it carries no privilege that one lacks.
        runtime.register_lazy_module("ansi", |lua| ansi::build(lua).map(Value::Table))?;
        // The stdlib modules arrive the same way, and none is built until a program requires it.
        for module in self.std_modules.modules() {
            runtime.register_lazy_module(module.name(), crate::stdlib::loader(module))?;
        }
        Ok(runtime)
    }
}

/// The executor a runtime drives its Lua on.
fn new_executor() -> Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| Error::Config(format!("could not start the tokio runtime: {e}")))
}

/// Ensures Lua's loaded-module table exists, and hands it back.
///
/// Lua creates it itself when it opens a standard library, so this normally just fetches it.
fn loaded_table(lua: &Lua) -> mlua::Result<Table> {
    match lua.named_registry_value::<Value>(LOADED)? {
        Value::Table(loaded) => Ok(loaded),
        _ => {
            let loaded = lua.create_table()?;
            lua.set_named_registry_value(LOADED, loaded.clone())?;
            Ok(loaded)
        }
    }
}

/// Applies a runtime's chunk rules to a chunk it is about to run.
///
/// The one rule is the binary-chunk refusal, and every chunk the runtime loads goes through
/// here — the script itself via [`Runtime::load`], and module source via [`load_from_store`] —
/// so the two cannot drift apart.
fn enforce_chunk_mode(chunk: Chunk<'_>, binary_chunks: bool) -> Chunk<'_> {
    if binary_chunks {
        chunk
    } else {
        chunk.set_mode(ChunkMode::Text)
    }
}

/// Installs our `require`, which resolves host modules and then the module store — and nothing
/// else. There is no `package.path` to point elsewhere and no searcher that reaches a `.so`.
fn install_require(lua: &Lua, modules: Arc<Modules>, binary_chunks: bool) -> Result<()> {
    loaded_table(lua)?;

    let require = lua.create_function(move |lua, name: String| {
        let name = ModuleName::new(name.clone())
            .map_err(|e| mlua::Error::runtime(format!("invalid module name {name:?}: {e}")))?;

        let loaded = loaded_table(lua)?;
        let cached: Value = loaded.raw_get(name.as_str())?;
        if !cached.is_nil() {
            return Ok(cached);
        }

        let _frame = LoadingFrame::enter(&modules, &name)?;

        let loader = lock(&modules.loaders).get(&name).cloned();
        let value = match loader {
            Some(loader) => loader(lua)?,
            None => load_from_store(lua, &modules, &name, binary_chunks)?,
        };

        // Lua's own `require` records a module that returns nothing as `true`, so that asking
        // for it again does not re-run it.
        let value = if value.is_nil() {
            Value::Boolean(true)
        } else {
            value
        };
        loaded.raw_set(name.as_str(), value.clone())?;
        Ok(value)
    })?;

    lua.globals().raw_set("require", require)?;
    Ok(())
}

/// Installs `stdlib`, which lists the stdlib modules this runtime registered, by the names
/// `require` takes them under.
///
/// The list is fixed here, when the runtime is built, and says what *this* runtime has: only the
/// pure modules in the sandbox profile, and no more than an embedder left in. It answers from the
/// selection rather than from what has been required, so asking builds nothing. Each call returns
/// a table of its own, so a script that edits the one it was given does not change the next
/// answer.
fn install_stdlib_list(lua: &Lua, modules: StdModules) -> Result<()> {
    let names: Vec<&'static str> = modules.modules().map(|module| module.name()).collect();
    let stdlib =
        lua.create_function(move |lua, ()| lua.create_sequence_from(names.iter().copied()))?;
    lua.globals().raw_set("stdlib", stdlib)?;
    Ok(())
}

fn load_from_store(
    lua: &Lua,
    modules: &Modules,
    name: &ModuleName,
    binary_chunks: bool,
) -> mlua::Result<Value> {
    let store = lock(&modules.store).clone();
    let Some(store) = store else {
        return Err(not_found(name, None));
    };
    let source = store
        .fetch(name)
        .map_err(|e| mlua::Error::runtime(format!("module '{name}' could not be read: {e}")))?;
    let Some(source) = source else {
        return Err(not_found(name, Some(&store.describe())));
    };

    let chunk = lua
        .load(source.source())
        .set_name(format!("@{}", source.origin()));
    // Lua passes the module its own name, so that one file can serve several names.
    enforce_chunk_mode(chunk, binary_chunks).call(name.as_str())
}

fn not_found(name: &ModuleName, store: Option<&str>) -> mlua::Error {
    let mut msg = format!("module '{name}' not found:\n\tno host module '{name}'");
    match store {
        Some(store) => msg.push_str(&format!("\n\tno source for '{name}' in {store}")),
        None => msg.push_str("\n\tno module store is configured"),
    }
    mlua::Error::runtime(msg)
}

/// Tracks what `require` is part-way through loading, so a cycle is reported rather than
/// recursing until the stack gives out.
struct LoadingFrame<'a> {
    modules: &'a Modules,
}

impl<'a> LoadingFrame<'a> {
    fn enter(modules: &'a Modules, name: &ModuleName) -> mlua::Result<Self> {
        let mut loading = lock(&modules.loading);
        if loading.contains(name) {
            let chain = loading
                .iter()
                .chain(std::iter::once(name))
                .map(ModuleName::as_str)
                .collect::<Vec<_>>()
                .join(" -> ");
            return Err(mlua::Error::runtime(format!(
                "module '{name}' is already loading (require cycle: {chain})"
            )));
        }
        loading.push(name.clone());
        drop(loading);
        Ok(LoadingFrame { modules })
    }
}

impl Drop for LoadingFrame<'_> {
    fn drop(&mut self) {
        lock(&self.modules.loading).pop();
    }
}

/// Installs a `debug` table holding only `traceback`.
///
/// The real `debug` library is not available on a safe Lua state, but `xpcall(f,
/// debug.traceback)` is how Lua code has always got a stack trace, and the underlying
/// `luaL_traceback` is a plain C API call that needs no library open. Code that feature-detects
/// on `debug.getinfo` will correctly find it missing.
fn install_traceback(lua: &Lua) -> Result<()> {
    let traceback = lua.create_function(|lua, (msg, level): (Option<String>, Option<usize>)| {
        lua.traceback(msg.as_deref(), level.unwrap_or(1))
    })?;
    let debug = lua.create_table()?;
    debug.raw_set("traceback", traceback)?;
    lua.globals().raw_set("debug", debug)?;
    Ok(())
}

/// Closes the two holes in Lua's base library: filesystem access, and binary chunks.
///
/// Loaded through [`Lua::load`] rather than [`Runtime::exec`] so that setting the runtime up is
/// never subject to the runtime's own limits.
///
/// `dofile` and `loadfile` read files despite living in the base library rather than in `io`,
/// so they follow `io`: present when it is, gone when it is not. `load` defaults to accepting
/// binary chunks, so unless binary chunks are allowed it is wrapped to force text mode — which
/// makes `load(bytecode, nil, "b")` fail with Lua's own "attempt to load a binary chunk"
/// message rather than something of our invention.
fn restrict_base_library(lua: &Lua, binary_chunks: bool) -> Result<()> {
    const DROP_FILE_FUNCTIONS: &str = r#"
        _G.dofile = nil
        _G.loadfile = nil
    "#;

    const TEXT_ONLY_CHUNKS: &str = r#"
        local load, loadfile = load, loadfile
        _G.load = function(chunk, chunkname, _mode, env)
            return load(chunk, chunkname, "t", env)
        end
        if loadfile then
            _G.loadfile = function(filename, _mode, env)
                return loadfile(filename, "t", env)
            end
            local text_loadfile = _G.loadfile
            _G.dofile = function(filename)
                local chunk, err = text_loadfile(filename)
                if not chunk then
                    error(err, 2)
                end
                return chunk()
            end
        end
    "#;

    let prelude = |source: &'static str| {
        let chunk = lua.load(source).set_name("=[avarice-rt base library]");
        enforce_chunk_mode(chunk, binary_chunks).exec()
    };

    if !lua.globals().contains_key("io")? {
        prelude(DROP_FILE_FUNCTIONS)?;
    }
    if !binary_chunks {
        prelude(TEXT_ONLY_CHUNKS)?;
    }
    Ok(())
}
