//! An embeddable Lua 5.4 runtime with a sandbox.
//!
//! avarice-rt wraps [`mlua`] with two things it does not provide: a sandbox that is actually
//! closed, and a module system the host controls.
//!
//! ```
//! use avarice_rt::{Profile, Runtime};
//!
//! let rt = Runtime::new(Profile::Sandbox)?;
//! let answer: i64 = rt.block_on(rt.eval("return 6 * 7", "=example"))?;
//! assert_eq!(answer, 42);
//! # Ok::<_, avarice_rt::Error>(())
//! ```
//!
//! # Running Lua
//!
//! There is one way to run a chunk and it is asynchronous, because a stdlib module may await
//! while Lua waits for it. [`Runtime::exec`] and [`Runtime::eval`] return futures, and the runtime
//! owns the current-thread tokio executor that drives them; [`Runtime::block_on`] runs a future on
//! it for a caller who is not async. It panics if called from inside another tokio runtime, so an
//! embedder that is already async builds its `Runtime` on a thread of its own.
//!
//! Lua can leave tasks running (the stdlib's `utils.spawn_task` and friends), and they run only
//! while the executor is being driven. See [`Runtime::outstanding_tasks`],
//! [`Runtime::wait_for_tasks`] and [`Runtime::abort_tasks`] for waiting on them and ending them.
//!
//! # Output
//!
//! Both profiles replace `print` with one that renders tables structurally. It writes to the
//! runtime's write sink, which is standard output until [`RuntimeBuilder::write_sink`] or
//! [`Runtime::set_write_sink`] says otherwise, and flushes after every call.
//!
//! # Profiles
//!
//! A [`Profile`] is a set of defaults, not a constraint. [`Profile::Sandbox`] withholds `io`,
//! `os`, `package` and `debug`, caps memory, and refuses binary chunks; [`Profile::Trusted`]
//! opens everything a safe Lua state can have and registers every stdlib module (`http`, `fs`,
//! `crypto`, `serde`, `datetime`, `utils`, `stores` and `validation`; see [`StdModules`]). [`RuntimeBuilder`] can override anything either
//! one sets, and doing so never affects the profile another runtime is built from.
//!
//! # Modules
//!
//! Lua's `package` library is never opened. `require` is ours, and resolves two things and
//! nothing else: modules the host registered from Rust, and then a [`ModuleStore`]. Lua code
//! cannot cause a `.so` to be loaded, cannot reach `package.path`, and cannot reach the
//! filesystem through the module system.
//!
//! ```
//! use avarice_rt::{FsStore, Profile, Runtime};
//!
//! let rt = Runtime::builder(Profile::Sandbox)
//!     .store(FsStore::new("/srv/lua"))
//!     .build()?;
//!
//! // A module implemented in Rust, available to the script as `require("clock")`.
//! let clock = rt.lua().create_table()?;
//! clock.set("now", rt.lua().create_function(|_, ()| Ok(0))?)?;
//! rt.register_module("clock", clock)?;
//! # Ok::<_, avarice_rt::Error>(())
//! ```
//!
//! A [`ModuleStore`] answers one question: given a module name, produce source or nothing. The
//! filesystem is one store and currently the only one shipped, but names are hierarchical by
//! convention rather than by path, so a store backed by a database table is equally valid.
//!
//! # Limits
//!
//! Memory is capped at the allocator. Wall-clock limits and cancellation are enforced from a
//! global debug hook, so a script cannot shed them by running inside a coroutine, and both
//! latch once tripped, so a `pcall` cannot swallow them. A [`CancelHandle`] is `Send`, and
//! can stop a runtime from another thread, whether it is running Lua or waiting on something.

mod error;
mod limits;
mod lock;
mod module;
mod print;
mod profile;
mod runtime;

pub use crate::error::{
    was_cancelled, was_out_of_memory, was_timed_out, Cancelled, Error, InvalidModuleName, Result,
    StoreError, TimedOut,
};
pub use crate::limits::{CancelHandle, Execution, DEFAULT_CHECK_INTERVAL};
pub use crate::module::{
    FsStore, ModuleName, ModuleSource, ModuleStore, MAX_NAME_LEN, MAX_NAME_SEGMENTS,
};
pub use crate::profile::{Profile, DEFAULT_SANDBOX_MEMORY_LIMIT};
pub use crate::runtime::{Runtime, RuntimeBuilder};
pub use avarice_rt_stdlib::{StdModule, StdModules};

/// mlua, re-exported.
///
/// Values, tables and functions crossing the boundary are mlua's, so an embedder needs the same
/// version avarice-rt was built against. It is a public dependency, and a breaking change to it
/// is a breaking change here.
pub use mlua;
