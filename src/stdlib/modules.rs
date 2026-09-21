//! Which stdlib modules there are, the set type an embedder selects them with, and how each one
//! is built.
//!
//! A module is the two layers Astra gives it: a Rust half that sets primitives on the Lua
//! globals under `astra_internal__*` names, and a Lua file, embedded with `include_str!`, that
//! wraps them into the module table and returns it. Both are Astra's, so `load` does no more
//! than Astra's own `register_components` and `require` do between them, once per module and
//! only when the module is first required, with one exception: see [`defines_globals`]. One
//! module, `stores`, has no Rust half at all: it is **pure**, and that is what sandbox mode
//! registers (ADR 0007).
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
/// contains is this directory's business and never the core's.
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
    /// Observables and pubsub. **Pure**: Lua only, with no Rust behind it.
    Stores,
    /// Schema validators, and regular expressions.
    Validation,
}

/// What is known about one module. Every fact about a module that is not code lives in [`TABLE`],
/// so that adding a module is one row here, one variant, one flag and one arm in
/// [`register_and_source`].
#[derive(Clone, Copy)]
struct Entry {
    module: StdModule,
    /// The name Lua `require`s it by.
    name: &'static str,
    /// The Cargo feature that compiles it in.
    feature: &'static str,
    flag: StdModules,
    /// Whether that feature is on in this build.
    compiled_in: bool,
    /// Whether the module is written entirely in Lua, with no Rust half. A pure module runs
    /// under every limit put on Lua, so sandbox mode registers it (ADR 0007). A module with a
    /// Rust half sets `astra_internal__*` primitives when built, and must say `false` here.
    pure: bool,
}

/// One row per module, compiled in or not, in the order [`StdModules::modules`] yields them. A
/// module's row is at the index of its discriminant, which `the_table_is_in_declaration_order`
/// holds to.
const TABLE: [Entry; 8] = [
    Entry {
        module: StdModule::Http,
        name: "http",
        feature: "stdlib-http",
        flag: StdModules::HTTP,
        compiled_in: cfg!(feature = "stdlib-http"),
        pure: false,
    },
    Entry {
        module: StdModule::Fs,
        name: "fs",
        feature: "stdlib-fs",
        flag: StdModules::FS,
        compiled_in: cfg!(feature = "stdlib-fs"),
        pure: false,
    },
    Entry {
        module: StdModule::Crypto,
        name: "crypto",
        feature: "stdlib-crypto",
        flag: StdModules::CRYPTO,
        compiled_in: cfg!(feature = "stdlib-crypto"),
        pure: false,
    },
    Entry {
        module: StdModule::Serde,
        name: "serde",
        feature: "stdlib-serde",
        flag: StdModules::SERDE,
        compiled_in: cfg!(feature = "stdlib-serde"),
        pure: false,
    },
    Entry {
        module: StdModule::Datetime,
        name: "datetime",
        feature: "stdlib-datetime",
        flag: StdModules::DATETIME,
        compiled_in: cfg!(feature = "stdlib-datetime"),
        pure: false,
    },
    Entry {
        module: StdModule::Utils,
        name: "utils",
        feature: "stdlib-utils",
        flag: StdModules::UTILS,
        compiled_in: cfg!(feature = "stdlib-utils"),
        pure: false,
    },
    Entry {
        module: StdModule::Stores,
        name: "stores",
        feature: "stdlib-stores",
        flag: StdModules::STORES,
        compiled_in: cfg!(feature = "stdlib-stores"),
        pure: true,
    },
    Entry {
        module: StdModule::Validation,
        name: "validation",
        feature: "stdlib-validation",
        flag: StdModules::VALIDATION,
        compiled_in: cfg!(feature = "stdlib-validation"),
        pure: false,
    },
];

const COMPILED_IN_COUNT: usize = {
    let mut count = 0;
    let mut i = 0;
    while i < TABLE.len() {
        if TABLE[i].compiled_in {
            count += 1;
        }
        i += 1;
    }
    count
};

/// The modules compiled in, taken from [`TABLE`] so that no second list has to agree with it.
const COMPILED_IN: [StdModule; COMPILED_IN_COUNT] = {
    let mut modules = [StdModule::Http; COMPILED_IN_COUNT];
    let mut next = 0;
    let mut i = 0;
    while i < TABLE.len() {
        if TABLE[i].compiled_in {
            modules[next] = TABLE[i].module;
            next += 1;
        }
        i += 1;
    }
    modules
};

impl StdModule {
    /// The modules compiled into this build, in the order [`StdModules::modules`] yields them.
    ///
    /// A build that turns a module's feature off has fewer than eight, so this is a slice and not
    /// a fixed array.
    pub const ALL: &'static [StdModule] = &COMPILED_IN;

    /// Whether this module is part of the build: whether [`StdModule::feature`] is on.
    ///
    /// A module that is not compiled in cannot be registered, by a profile or by anyone.
    pub const fn is_compiled_in(self) -> bool {
        self.entry().compiled_in
    }

    /// The Cargo feature that compiles this module in.
    pub const fn feature(self) -> &'static str {
        self.entry().feature
    }

    /// The name Lua passes to `require` to get this module.
    pub const fn name(self) -> &'static str {
        self.entry().name
    }

    const fn entry(self) -> Entry {
        TABLE[self as usize]
    }
}

bitflags! {
    /// A set of [`StdModule`]s: which of them a runtime registers.
    ///
    /// One flag per module, named for the module, in every build. [`StdModules::ALL`] and
    /// [`StdModules::NONE`] are the two a profile starts from. A set may name a module that is not
    /// compiled in; [`StdModules::not_compiled_in`] says which.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct StdModules: u16 {
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
        while i < TABLE.len() {
            if TABLE[i].compiled_in {
                set = set.union(TABLE[i].flag);
            }
            i += 1;
        }
        set
    };

    /// Every stdlib module that is compiled in and pure: written in Lua with no Rust behind it.
    ///
    /// This is what sandbox mode registers. A pure module reaches nothing outside the Lua state,
    /// so the memory cap and the time limit govern it, which cannot be said of a module with
    /// Rust behind it (ADR 0007). Purity is a fact recorded once per module, so this is derived
    /// and not a second list.
    pub const PURE: StdModules = {
        let mut set = StdModules::empty();
        let mut i = 0;
        while i < TABLE.len() {
            if TABLE[i].compiled_in && TABLE[i].pure {
                set = set.union(TABLE[i].flag);
            }
            i += 1;
        }
        set
    };

    /// No stdlib module.
    pub const NONE: StdModules = StdModules::empty();

    /// The modules in this set, in a fixed order, compiled in or not.
    pub fn modules(self) -> impl Iterator<Item = StdModule> {
        TABLE
            .into_iter()
            .filter(move |entry| self.contains(entry.flag))
            .map(|entry| entry.module)
    }

    /// The modules in this set that are not compiled in, and so cannot be registered.
    pub const fn not_compiled_in(self) -> StdModules {
        self.difference(StdModules::ALL)
    }

    /// `Ok` if every module in this set is compiled in, and otherwise an error naming the ones
    /// that are not and the features that would compile them in.
    pub fn require_compiled_in(self) -> Result<(), NotCompiledIn> {
        match self.not_compiled_in() {
            missing if missing.is_empty() => Ok(()),
            missing => Err(NotCompiledIn(missing)),
        }
    }
}

/// A selection named stdlib modules that are not compiled in. Its message says which, and which
/// Cargo features compile them in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotCompiledIn(StdModules);

impl std::fmt::Display for NotCompiledIn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names: Vec<_> = self
            .0
            .modules()
            .map(|m| format!("'{}'", m.name()))
            .collect();
        let features: Vec<_> = self
            .0
            .modules()
            .map(|m| format!("`{}`", m.feature()))
            .collect();
        let (s, verb) = if names.len() == 1 {
            ("", "is")
        } else {
            ("s", "are")
        };
        write!(
            f,
            "the stdlib module{s} {} {verb} not compiled in: build with the {} feature{s} \
             (`stdlib` turns them all on)",
            names.join(", "),
            features.join(", "),
        )
    }
}

impl std::error::Error for NotCompiledIn {}

impl From<StdModule> for StdModules {
    fn from(module: StdModule) -> Self {
        module.entry().flag
    }
}

impl FromIterator<StdModule> for StdModules {
    fn from_iter<I: IntoIterator<Item = StdModule>>(modules: I) -> Self {
        modules.into_iter().fold(StdModules::NONE, |set, module| {
            set | StdModules::from(module)
        })
    }
}

/// Registers `module`'s Rust primitives, and returns the Lua source of its wrapper.
///
/// A module that is not compiled in is an error naming the feature, not a panic: [`loader`] is
/// public, and hands out a loader for any variant.
///
/// [`loader`]: super::loader
fn register_and_source(lua: &Lua, module: StdModule) -> mlua::Result<&'static str> {
    // `lua` is only used by a module that is compiled in.
    let _ = lua;
    match module {
        #[cfg(feature = "stdlib-http")]
        StdModule::Http => {
            super::components::http::client::HTTPClientRequest::register_to_lua(lua)?;
            Ok(include_str!("lua/http.lua"))
        }
        #[cfg(feature = "stdlib-fs")]
        StdModule::Fs => {
            super::components::file_system::register_to_lua(lua)?;
            super::components::file_system::GlobResult::register_to_lua(lua)?;
            Ok(include_str!("lua/fs.lua"))
        }
        #[cfg(feature = "stdlib-crypto")]
        StdModule::Crypto => {
            super::components::crypto::register_to_lua(lua)?;
            Ok(include_str!("lua/crypto.lua"))
        }
        #[cfg(feature = "stdlib-serde")]
        StdModule::Serde => {
            super::components::astra_serde::register_to_lua(lua)?;
            Ok(include_str!("lua/serde.lua"))
        }
        #[cfg(feature = "stdlib-datetime")]
        StdModule::Datetime => {
            super::components::datetime::AstraDateTime::register_to_lua(lua)?;
            Ok(include_str!("lua/datetime.lua"))
        }
        #[cfg(feature = "stdlib-utils")]
        StdModule::Utils => {
            super::components::utils::register_to_lua(lua)?;
            Ok(include_str!("lua/utils.lua"))
        }
        // Astra's `stores` has no Rust half: it is Lua all the way down, and so it is pure.
        #[cfg(feature = "stdlib-stores")]
        StdModule::Stores => Ok(include_str!("lua/stores.lua")),
        // The regex primitive is set by Astra's `utils` Rust half, alongside the tasks, so
        // `validation` sets it too rather than lean on `utils` having been built first. Setting
        // it twice is harmless.
        #[cfg(feature = "stdlib-validation")]
        StdModule::Validation => {
            super::components::utils::AstraRegex::register_to_lua(lua)?;
            Ok(include_str!("lua/validation.lua"))
        }
        // Only reached by a module whose feature is off; in a build with every feature on, every
        // variant has an arm above.
        #[allow(unreachable_patterns)]
        module => Err(mlua::Error::runtime(NotCompiledIn(module.into()))),
    }
}

/// Builds `module`'s value: registers its primitives, then runs its Lua layer.
pub(crate) fn load(lua: &Lua, module: StdModule) -> mlua::Result<Value> {
    let source = register_and_source(lua, module)?;
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
        for module in StdModules::all().modules() {
            assert!(!StdModules::NONE.contains(module.into()));
        }
    }

    #[test]
    fn every_module_is_exactly_one_flag() {
        // The flag set and the enum cannot drift apart: each variant maps to a distinct single
        // flag, and all of them together are every flag, compiled in or not.
        let mut union = StdModules::NONE;
        for module in StdModules::all().modules() {
            let flag = StdModules::from(module);
            assert_eq!(flag.bits().count_ones(), 1, "{module:?}");
            assert!(!union.intersects(flag), "{module:?} shares a bit");
            union |= flag;
        }
        assert_eq!(union, StdModules::all());
    }

    #[test]
    fn the_table_is_in_declaration_order() {
        // `StdModule::entry` indexes the table by discriminant, and `modules` yields it in order.
        for (index, entry) in TABLE.iter().enumerate() {
            assert_eq!(entry.module as usize, index, "{}", entry.name);
        }
    }

    #[test]
    fn a_features_name_is_the_module_prefixed() {
        for entry in TABLE {
            assert_eq!(entry.feature, format!("stdlib-{}", entry.name));
        }
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
    fn the_pure_module_is_stores() {
        // A tripwire, not a second list to keep in step: giving it a Rust half is a decision
        // (ADR 0007), so the test that says which are pure has to be changed with it.
        let pure: Vec<_> = TABLE.iter().filter(|e| e.pure).map(|e| e.name).collect();
        assert_eq!(pure, ["stores"]);
    }

    #[test]
    fn pure_is_the_pure_modules_that_are_compiled_in() {
        assert!(StdModules::ALL.contains(StdModules::PURE));
        for entry in TABLE {
            assert_eq!(
                StdModules::PURE.contains(entry.flag),
                entry.pure && entry.compiled_in,
                "{}",
                entry.name
            );
        }
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
