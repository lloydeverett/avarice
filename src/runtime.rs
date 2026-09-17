//! The runtime, and the builder that configures one.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use mlua::chunk::{AsChunk, Chunk, ChunkMode};
use mlua::{FromLuaMulti, IntoLua, Lua, LuaOptions, StdLib, Table, Value};

use crate::error::{Error, Result};
use crate::limits::{self, CancelHandle, Execution, Limits, DEFAULT_CHECK_INTERVAL};
use crate::module::{ModuleName, ModuleStore};
use crate::profile::Profile;

/// The registry key Lua itself uses for its loaded-module table.
const LOADED: &str = "_LOADED";

type Loader = Box<dyn Fn(&Lua) -> mlua::Result<Value>>;

/// What `require` resolves against, mutable after the runtime is built.
#[derive(Default)]
struct Modules {
    store: RefCell<Option<Arc<dyn ModuleStore>>>,
    loaders: RefCell<HashMap<String, Rc<Loader>>>,
    loading: RefCell<Vec<String>>,
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
    limits: Rc<Limits>,
    modules: Rc<Modules>,
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
        let execution = Execution::new(Rc::clone(&self.limits));
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
        if self.binary_chunks {
            chunk
        } else {
            chunk.set_mode(ChunkMode::Text)
        }
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
    pub fn register_lazy_module<F>(&self, name: &str, loader: F) -> Result<()>
    where
        F: Fn(&Lua) -> mlua::Result<Value> + 'static,
    {
        let name = ModuleName::new(name)?;
        self.modules
            .loaders
            .borrow_mut()
            .insert(name.into(), Rc::new(Box::new(loader) as Loader));
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
        if self.modules.loaders.borrow().contains_key(name.as_str()) {
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
        *self.modules.store.borrow_mut() = Some(Arc::new(store));
    }

    /// Removes the module store, leaving only host-registered modules resolvable.
    pub fn clear_store(&self) {
        *self.modules.store.borrow_mut() = None;
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
    /// [`StdLib::ALL_SAFE`] is a trap here: for Lua (as opposed to Luau) it includes
    /// `package`, which would hand Lua back the module loading this runtime exists to take away.
    pub fn std_libs(mut self, libs: StdLib) -> Self {
        self.std_libs = libs;
        self
    }

    /// Opens these libraries in addition to the profile's.
    pub fn with_std_libs(mut self, libs: StdLib) -> Self {
        self.std_libs |= libs;
        self
    }

    /// Withholds these libraries from the profile's set.
    pub fn without_std_libs(mut self, libs: StdLib) -> Self {
        self.std_libs &= !libs;
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
        // mlua rejects both of these on a safe state, and we never reach for the unsafe
        // constructor: it exists to let *Lua* load C modules, which is the one thing this
        // runtime is built not to allow.
        if self.std_libs.contains(StdLib::DEBUG) {
            return Err(Error::Config(
                "the debug library cannot be opened: parts of it can violate mlua's safety \
                 invariants, so a `debug` table carrying only `traceback` is installed instead"
                    .to_string(),
            ));
        }
        let lua = Lua::new_with(self.std_libs, LuaOptions::default())?;

        let modules = Rc::new(Modules {
            store: RefCell::new(self.store),
            ..Modules::default()
        });
        let limits = Rc::new(Limits::new(self.cancel, self.time_limit));

        let runtime = Runtime {
            lua,
            limits: Rc::clone(&limits),
            modules: Rc::clone(&modules),
            profile: self.profile,
            binary_chunks: self.binary_chunks,
            memory_limit: self.memory_limit,
        };

        install_require(&runtime, modules)?;
        install_traceback(&runtime)?;
        restrict_base_library(&runtime)?;

        // Last, so that none of our own setup can trip the limits we install.
        if let Some(bytes) = self.memory_limit {
            runtime.lua.set_memory_limit(bytes)?;
        }
        if limits.needs_hook() {
            limits::install_hook(&runtime.lua, limits, self.check_interval)?;
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

/// Installs our `require`, which resolves host modules and then the module store — and nothing
/// else. There is no `package.path` to point elsewhere and no searcher that reaches a `.so`.
fn install_require(runtime: &Runtime, modules: Rc<Modules>) -> Result<()> {
    let lua = &runtime.lua;
    loaded_table(lua)?;

    let binary_chunks = runtime.binary_chunks;
    let require = lua.create_function(move |lua, name: String| {
        let name = ModuleName::new(name.clone())
            .map_err(|e| mlua::Error::runtime(format!("invalid module name {name:?}: {e}")))?;

        let loaded = loaded_table(lua)?;
        let cached: Value = loaded.raw_get(name.as_str())?;
        if !cached.is_nil() {
            return Ok(cached);
        }

        let _frame = LoadingFrame::enter(&modules, &name)?;

        let loader = modules.loaders.borrow().get(name.as_str()).cloned();
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
    let store = modules.store.borrow().clone();
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
    let chunk = if binary_chunks {
        chunk
    } else {
        chunk.set_mode(ChunkMode::Text)
    };
    // Lua passes the module its own name, so that one file can serve several names.
    chunk.call(name.as_str())
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
        let mut loading = modules.loading.borrow_mut();
        if loading.iter().any(|n| n == name.as_str()) {
            let mut chain = loading.join(" -> ");
            chain.push_str(" -> ");
            chain.push_str(name.as_str());
            return Err(mlua::Error::runtime(format!(
                "module '{name}' is already loading (require cycle: {chain})"
            )));
        }
        loading.push(name.as_str().to_owned());
        drop(loading);
        Ok(LoadingFrame { modules })
    }
}

impl Drop for LoadingFrame<'_> {
    fn drop(&mut self) {
        self.modules.loading.borrow_mut().pop();
    }
}

/// Installs a `debug` table holding only `traceback`.
///
/// The real `debug` library is not available on a safe Lua state, but `xpcall(f,
/// debug.traceback)` is how Lua code has always got a stack trace, and the underlying
/// `luaL_traceback` is a plain C API call that needs no library open. Code that feature-detects
/// on `debug.getinfo` will correctly find it missing.
fn install_traceback(runtime: &Runtime) -> Result<()> {
    let lua = &runtime.lua;
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
/// Loaded through [`Runtime::load`] rather than [`Runtime::exec`] so that setting the runtime up
/// is never subject to the runtime's own limits.
///
/// `dofile` and `loadfile` read files despite living in the base library rather than in `io`,
/// so they follow `io`: present when it is, gone when it is not. `load` defaults to accepting
/// binary chunks, so unless binary chunks are allowed it is wrapped to force text mode — which
/// makes `load(bytecode, nil, "b")` fail with Lua's own "attempt to load a binary chunk"
/// message rather than something of our invention.
fn restrict_base_library(runtime: &Runtime) -> Result<()> {
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

    let has_io = runtime.lua.globals().contains_key("io")?;
    if !has_io {
        runtime
            .load(DROP_FILE_FUNCTIONS, "=[avarice-rt base library]")
            .exec()?;
    }
    if !runtime.binary_chunks {
        runtime
            .load(TEXT_ONLY_CHUNKS, "=[avarice-rt base library]")
            .exec()?;
    }
    Ok(())
}
