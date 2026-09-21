//! Which stdlib modules there are, the set type an embedder selects them with, and how each one
//! is built.
//!
//! A module is the two layers Astra gives it: a Rust half that sets primitives on the Lua
//! globals under `astra_internal__*` names, and a Lua file, embedded with `include_str!`, that
//! wraps them into the module table and returns it. Both are Astra's, so `load` does no more
//! than Astra's own `register_components` and `require` do between them, once per module and
//! only when the module is first required, with one exception: see [`defines_globals`].
//!
//! Every module is behind a Cargo feature that compiles it in (ADR 0007). The types here have all
//! eight modules in every build, so what an embedder matches on does not vary with features; what
//! varies is which of them are *compiled in*, and asking for one that is not is an error rather
//! than a silent omission.

use bitflags::bitflags;
use mlua::{Lua, Table, Value};

/// One of the stdlib modules.
///
/// Each knows the name Lua `require`s it by. The set of modules is this enum; what a module
/// contains is this crate's business and never the core's.
///
/// Every variant exists in every build. A module is usable only if it is **compiled in**, which
/// its Cargo feature (see [`StdModule::feature`]) decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum StdModule {
    /// An HTTP client (Astra's server is not taken).
    Http,
    /// Files and directories.
    Fs,
    /// SHA-2 and SHA-3 hashing and base64.
    Crypto,
    /// JSON, JSON5, YAML, TOML, INI, CSV and XML encoding and decoding.
    Serde,
    /// Dates and times, built on chrono.
    Datetime,
    /// Tasks, `uuid` and `env.get`.
    Utils,
    /// Observables and pubsub.
    Stores,
    /// Schema validators, and regular expressions.
    Validation,
}

/// Every module, compiled in or not, in the order [`StdModules::modules`] yields them.
const EVERY: [StdModule; 8] = [
    StdModule::Http,
    StdModule::Fs,
    StdModule::Crypto,
    StdModule::Serde,
    StdModule::Datetime,
    StdModule::Utils,
    StdModule::Stores,
    StdModule::Validation,
];

impl StdModule {
    /// The modules compiled into this build, in the order [`StdModules::modules`] yields them.
    ///
    /// A build that turns a module's feature off has fewer than eight, so this is a slice and not
    /// a fixed array.
    pub const ALL: &'static [StdModule] = &[
        #[cfg(feature = "stdlib-http")]
        StdModule::Http,
        #[cfg(feature = "stdlib-fs")]
        StdModule::Fs,
        #[cfg(feature = "stdlib-crypto")]
        StdModule::Crypto,
        #[cfg(feature = "stdlib-serde")]
        StdModule::Serde,
        #[cfg(feature = "stdlib-datetime")]
        StdModule::Datetime,
        #[cfg(feature = "stdlib-utils")]
        StdModule::Utils,
        #[cfg(feature = "stdlib-stores")]
        StdModule::Stores,
        #[cfg(feature = "stdlib-validation")]
        StdModule::Validation,
    ];

    /// Whether this module is part of the build: whether [`StdModule::feature`] is on.
    ///
    /// A module that is not compiled in cannot be registered, by a profile or by anyone.
    pub const fn is_compiled_in(self) -> bool {
        match self {
            StdModule::Http => cfg!(feature = "stdlib-http"),
            StdModule::Fs => cfg!(feature = "stdlib-fs"),
            StdModule::Crypto => cfg!(feature = "stdlib-crypto"),
            StdModule::Serde => cfg!(feature = "stdlib-serde"),
            StdModule::Datetime => cfg!(feature = "stdlib-datetime"),
            StdModule::Utils => cfg!(feature = "stdlib-utils"),
            StdModule::Stores => cfg!(feature = "stdlib-stores"),
            StdModule::Validation => cfg!(feature = "stdlib-validation"),
        }
    }

    /// The Cargo feature that compiles this module in, spelled the same on this crate and on
    /// `avarice-rt`.
    pub const fn feature(self) -> &'static str {
        match self {
            StdModule::Http => "stdlib-http",
            StdModule::Fs => "stdlib-fs",
            StdModule::Crypto => "stdlib-crypto",
            StdModule::Serde => "stdlib-serde",
            StdModule::Datetime => "stdlib-datetime",
            StdModule::Utils => "stdlib-utils",
            StdModule::Stores => "stdlib-stores",
            StdModule::Validation => "stdlib-validation",
        }
    }

    /// The name Lua passes to `require` to get this module.
    pub const fn name(self) -> &'static str {
        match self {
            StdModule::Http => "http",
            StdModule::Fs => "fs",
            StdModule::Crypto => "crypto",
            StdModule::Serde => "serde",
            StdModule::Datetime => "datetime",
            StdModule::Utils => "utils",
            StdModule::Stores => "stores",
            StdModule::Validation => "validation",
        }
    }
}

bitflags! {
    /// A set of [`StdModule`]s: which of them a runtime registers.
    ///
    /// One flag per module, named for the module, in every build. [`StdModules::ALL`] and
    /// [`StdModules::NONE`] are the two a profile starts from. A set may name a module that is not
    /// compiled in; [`StdModules::not_compiled_in`] says which.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct StdModules: u8 {
        /// [`StdModule::Http`].
        const HTTP = 1 << 0;
        /// [`StdModule::Fs`].
        const FS = 1 << 1;
        /// [`StdModule::Crypto`].
        const CRYPTO = 1 << 2;
        /// [`StdModule::Serde`].
        const SERDE = 1 << 3;
        /// [`StdModule::Datetime`].
        const DATETIME = 1 << 4;
        /// [`StdModule::Utils`].
        const UTILS = 1 << 5;
        /// [`StdModule::Stores`].
        const STORES = 1 << 6;
        /// [`StdModule::Validation`].
        const VALIDATION = 1 << 7;
    }
}

impl StdModules {
    /// Every stdlib module that is compiled in.
    ///
    /// This is every module unless the build turned some off, and it is what trusted mode
    /// registers. It is not [`StdModules::all`], which is every flag whether or not the module
    /// behind it is compiled in.
    pub const ALL: StdModules = {
        let mut set = StdModules::empty();
        let mut i = 0;
        while i < EVERY.len() {
            if EVERY[i].is_compiled_in() {
                set = set.union(EVERY[i].flag());
            }
            i += 1;
        }
        set
    };

    /// No stdlib module.
    pub const NONE: StdModules = StdModules::empty();

    /// The modules in this set, in a fixed order, compiled in or not.
    pub fn modules(self) -> impl Iterator<Item = StdModule> {
        EVERY
            .into_iter()
            .filter(move |module| self.contains(module.flag()))
    }

    /// The modules in this set that are not compiled in, and so cannot be registered.
    pub const fn not_compiled_in(self) -> StdModules {
        self.difference(StdModules::ALL)
    }
}

impl StdModule {
    const fn flag(self) -> StdModules {
        match self {
            StdModule::Http => StdModules::HTTP,
            StdModule::Fs => StdModules::FS,
            StdModule::Crypto => StdModules::CRYPTO,
            StdModule::Serde => StdModules::SERDE,
            StdModule::Datetime => StdModules::DATETIME,
            StdModule::Utils => StdModules::UTILS,
            StdModule::Stores => StdModules::STORES,
            StdModule::Validation => StdModules::VALIDATION,
        }
    }
}

impl From<StdModule> for StdModules {
    fn from(module: StdModule) -> Self {
        module.flag()
    }
}

impl FromIterator<StdModule> for StdModules {
    fn from_iter<I: IntoIterator<Item = StdModule>>(modules: I) -> Self {
        modules.into_iter().fold(StdModules::NONE, |set, module| {
            set | StdModules::from(module)
        })
    }
}

/// The Lua source of `module`'s wrapper, once its Rust half has registered its primitives.
///
/// A module that is not compiled in is an error naming the feature, not a panic: [`loader`] is
/// public, and hands out a loader for any variant.
///
/// [`loader`]: crate::loader
fn source(lua: &Lua, module: StdModule) -> mlua::Result<&'static str> {
    // `lua` is only used by a module that is compiled in.
    let _ = lua;
    match module {
        #[cfg(feature = "stdlib-http")]
        StdModule::Http => {
            crate::components::http::client::HTTPClientRequest::register_to_lua(lua)?;
            Ok(include_str!("../lua/http.lua"))
        }
        #[cfg(feature = "stdlib-fs")]
        StdModule::Fs => {
            crate::components::file_system::register_to_lua(lua)?;
            crate::components::file_system::GlobResult::register_to_lua(lua)?;
            Ok(include_str!("../lua/fs.lua"))
        }
        #[cfg(feature = "stdlib-crypto")]
        StdModule::Crypto => {
            crate::components::crypto::register_to_lua(lua)?;
            Ok(include_str!("../lua/crypto.lua"))
        }
        #[cfg(feature = "stdlib-serde")]
        StdModule::Serde => {
            crate::components::astra_serde::register_to_lua(lua)?;
            Ok(include_str!("../lua/serde.lua"))
        }
        #[cfg(feature = "stdlib-datetime")]
        StdModule::Datetime => {
            crate::components::datetime::AstraDateTime::register_to_lua(lua)?;
            Ok(include_str!("../lua/datetime.lua"))
        }
        #[cfg(feature = "stdlib-utils")]
        StdModule::Utils => {
            crate::components::utils::register_to_lua(lua)?;
            Ok(include_str!("../lua/utils.lua"))
        }
        // Astra's `stores` has no Rust half: it is Lua all the way down.
        #[cfg(feature = "stdlib-stores")]
        StdModule::Stores => Ok(include_str!("../lua/stores.lua")),
        // The regex primitive is set by Astra's `utils` Rust half, alongside the tasks, so
        // `validation` sets it too rather than lean on `utils` having been built first. Setting
        // it twice is harmless.
        #[cfg(feature = "stdlib-validation")]
        StdModule::Validation => {
            crate::components::utils::AstraRegex::register_to_lua(lua)?;
            Ok(include_str!("../lua/validation.lua"))
        }
        // Only reached by a module whose feature is off; in a build with every feature on, every
        // variant has an arm above.
        #[allow(unreachable_patterns)]
        module => Err(mlua::Error::runtime(format!(
            "the stdlib module '{}' is not compiled in: build with the `{}` feature",
            module.name(),
            module.feature()
        ))),
    }
}

/// Builds `module`'s value: registers its primitives, then runs its Lua layer.
pub(crate) fn load(lua: &Lua, module: StdModule) -> mlua::Result<Value> {
    let source = source(lua, module)?;
    // Named so a traceback through a stdlib module says where it came from. Text only: the
    // sources are embedded, and a chunk that is not text is not one of ours.
    let chunk = lua
        .load(source)
        .set_name(format!("=[avarice-rt stdlib {}]", module.name()))
        .set_mode(mlua::chunk::ChunkMode::Text);
    if defines_globals(module) {
        chunk.set_environment(private_globals(lua)?).eval()
    } else {
        chunk.eval()
    }
}

/// Whether the module's Lua file declares its functions as *global* ones.
///
/// Astra's `validation.lua` does: `number`, `struct`, `regex` and a dozen more. Run as a plain
/// chunk it would put them in every program's globals as soon as anything required it, and would
/// break the day a program reused one of those names. Running it against a table of its own keeps
/// the file as it is. This is the only place a module is loaded other than Astra's way.
const fn defines_globals(module: StdModule) -> bool {
    matches!(module, StdModule::Validation)
}

/// A table for a chunk to treat as its globals: it reads through to the real ones, but what the
/// chunk defines stays in the table, and out of the program's.
fn private_globals(lua: &Lua) -> mlua::Result<Table> {
    let env = lua.create_table()?;
    let read_through = lua.create_table()?;
    read_through.set("__index", lua.globals())?;
    env.set_metatable(Some(read_through))?;
    Ok(env)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_what_lua_requires_them_by() {
        let names: Vec<_> = StdModules::all().modules().map(StdModule::name).collect();
        assert_eq!(
            names,
            [
                "http",
                "fs",
                "crypto",
                "serde",
                "datetime",
                "utils",
                "stores",
                "validation"
            ]
        );
    }

    #[test]
    fn none_is_no_module_and_all_is_no_more_than_every_module() {
        assert_eq!(StdModules::NONE.modules().count(), 0);
        assert!(StdModules::all().contains(StdModules::ALL));
        for module in EVERY {
            assert!(!StdModules::NONE.contains(module.into()));
        }
    }

    #[test]
    fn every_module_is_exactly_one_flag() {
        // The flag set and the enum cannot drift apart: each variant maps to a distinct single
        // flag, and all of them together are every flag, compiled in or not.
        let mut union = StdModules::NONE;
        for module in EVERY {
            let flag = StdModules::from(module);
            assert_eq!(flag.bits().count_ones(), 1, "{module:?}");
            assert!(!union.intersects(flag), "{module:?} shares a bit");
            union |= flag;
        }
        assert_eq!(union, StdModules::all());
    }

    #[test]
    fn bit_values_do_not_move_with_features() {
        // An embedder may keep the bits, and a feature must not change what they mean.
        assert_eq!(StdModules::HTTP.bits(), 1);
        assert_eq!(StdModules::FS.bits(), 1 << 1);
        assert_eq!(StdModules::CRYPTO.bits(), 1 << 2);
        assert_eq!(StdModules::SERDE.bits(), 1 << 3);
        assert_eq!(StdModules::DATETIME.bits(), 1 << 4);
        assert_eq!(StdModules::UTILS.bits(), 1 << 5);
        assert_eq!(StdModules::STORES.bits(), 1 << 6);
        assert_eq!(StdModules::VALIDATION.bits(), 1 << 7);
    }

    #[test]
    fn modules_yields_the_set_variants_in_a_fixed_order() {
        let set = StdModules::VALIDATION | StdModules::HTTP | StdModules::CRYPTO;
        let modules: Vec<_> = set.modules().collect();
        assert_eq!(
            modules,
            [StdModule::Http, StdModule::Crypto, StdModule::Validation]
        );
    }

    #[test]
    fn set_algebra_adds_and_subtracts() {
        let without_http = StdModules::all() - StdModules::HTTP;
        assert!(!without_http.contains(StdModules::HTTP));
        assert_eq!(without_http.modules().count(), 7);
        assert_eq!(without_http | StdModules::HTTP, StdModules::all());
        assert_eq!(StdModules::all() & !StdModules::all(), StdModules::NONE);
    }

    #[test]
    fn a_set_collects_from_modules() {
        let set: StdModules = [StdModule::Fs, StdModule::Utils].into_iter().collect();
        assert_eq!(set, StdModules::FS | StdModules::UTILS);
    }
}
