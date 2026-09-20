//! The runtime, and the builder that configures one.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use avarice_rt_stdlib::StdModules;
use mlua::chunk::{AsChunk, Chunk, ChunkMode};
use mlua::{FromLuaMulti, IntoLua, Lua, LuaOptions, StdLib, Table, Value};

use crate::error::{Error, Result};
use crate::limits::{self, CancelHandle, Execution, Limits, DEFAULT_CHECK_INTERVAL};
use crate::module::{ModuleName, ModuleStore};
use crate::profile::Profile;

/// The registry key Lua itself uses for its loaded-module table.
const LOADED: &str = "_LOADED";

// `Send + Sync` because mlua's `send` feature, which the stdlib crate needs for its tasks, makes
// the `require` function Lua holds `Send`, and `require` reaches the loaders.
type Loader = Arc<dyn Fn(&Lua) -> mlua::Result<Value> + Send + Sync>;

/// Locks a mutex, carrying on if a panic elsewhere poisoned it. Nothing under these locks is
/// held across a call into Lua or a loader, so a panic cannot leave one half-updated.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What `require` resolves against, mutable after the runtime is built.
#[derive(Default)]
struct Modules {
    store: Mutex<Option<Arc<dyn ModuleStore>>>,
    loaders: Mutex<HashMap<ModuleName, Loader>>,
    loading: Mutex<Vec<ModuleName>>,
}

/// A Lua runtime: a Lua state, the libraries a [`Profile`] granted it, and the limits it runs
/// under.
///
/// ```
/// use avarice_rt::{Profile, Runtime};
///
/// let rt = Runtime::new(Profile::Sandbox)?;
/// let sum: i64 = rt.eval("1 + 2", "=example")?;
/// assert_eq!(sum, 3);
/// # Ok::<_, avarice_rt::Error>(())
/// ```
pub struct Runtime {
    lua: Lua,
    limits: Arc<Limits>,
    modules: Arc<Modules>,
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

    /// The underlying Lua state, for everything this API does not wrap.
    ///
    /// Two things to know when driving Lua directly through this handle. Chunks loaded with
    /// [`Lua::load`] do not inherit this runtime's refusal of binary chunks — use
    /// [`Runtime::load`] for that. And a time limit is armed only while an
    /// [`Execution`] guard is held, so take one from [`Runtime::enter`] around calls made from
    /// here; cancellation needs no arming and applies throughout.
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
    /// [`exec`](Self::exec) and [`eval`](Self::eval) do this for themselves; call it when
    /// driving Lua through [`Runtime::lua`]. Nesting is safe: an inner guard does not extend the
    /// outer execution's budget.
    ///
    /// Fails immediately if the runtime's [`CancelHandle`] is already tripped, so that a chunk
    /// too short to reach a single hook tick cannot slip past a cancel.
    pub fn enter(&self) -> Result<Execution> {
        let execution = Execution::new(Arc::clone(&self.limits));
        match self.limits.precheck() {
            Some(err) => Err(err.into()),
            None => Ok(execution),
        }
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

    /// Runs a chunk for its side effects.
    pub fn exec(&self, source: impl AsChunk, name: impl Into<String>) -> Result<()> {
        let _exec = self.enter()?;
        self.load(source, name).exec()?;
        Ok(())
    }

    /// Runs a chunk and converts its result.
    ///
    /// A chunk that parses as an expression is evaluated as one, so `eval::<i64>("1 + 2")`
    /// works as well as `eval::<()>("x = 1")`.
    pub fn eval<R: FromLuaMulti>(
        &self,
        source: impl AsChunk,
        name: impl Into<String>,
    ) -> Result<R> {
        let _exec = self.enter()?;
        Ok(self.load(source, name).eval()?)
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
    /// The loader must be `Send + Sync`: Lua's `require` holds it, and the stdlib crate turns on
    /// mlua's `send` feature, which makes everything Lua holds `Send`.
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
    pub fn std_modules(mut self, modules: StdModules) -> Self {
        self.std_modules = modules;
        self
    }

    /// Adds stdlib modules to the set the runtime registers.
    pub fn with_std_modules(mut self, modules: StdModules) -> Self {
        self.std_modules |= modules;
        self
    }

    /// Removes stdlib modules from the set the runtime registers.
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
    /// The clock starts when [`Runtime::exec`], [`Runtime::eval`] or [`Runtime::enter`] is
    /// called and stops when it returns, so the limit is per execution rather than for the
    /// runtime's lifetime. Installs the limit hook, which costs a little throughput.
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
        let lua = Lua::new_with(self.std_libs, LuaOptions::default())?;

        let modules = Arc::new(Modules {
            store: Mutex::new(self.store),
            ..Modules::default()
        });
        let limits = Arc::new(Limits::new(self.cancel, self.time_limit));

        install_require(&lua, Arc::clone(&modules), self.binary_chunks)?;
        install_traceback(&lua)?;
        restrict_base_library(&lua, self.binary_chunks)?;

        // Last, so that none of our own setup can trip the limits we install.
        if let Some(bytes) = self.memory_limit {
            lua.set_memory_limit(bytes)?;
        }
        if limits.needs_hook() {
            limits::install_hook(&lua, Arc::clone(&limits), self.check_interval)?;
        }

        let runtime = Runtime {
            lua,
            limits,
            modules,
            profile: self.profile,
            binary_chunks: self.binary_chunks,
            memory_limit: self.memory_limit,
        };

        // The stdlib modules arrive by the path an embedder's own lazy module takes, so they
        // carry no privilege that one lacks, and none is built until a program requires it.
        for module in self.std_modules.modules() {
            runtime.register_lazy_module(module.name(), avarice_rt_stdlib::loader(module))?;
        }
        Ok(runtime)
    }
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
