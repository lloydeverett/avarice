//! Which stdlib modules there are, the set type an embedder selects them with, and how each one
//! is built.
//!
//! A module is the two layers Astra gives it: a Rust half that sets primitives on the Lua
//! globals under `astra_internal__*` names, and a Lua file, embedded with `include_str!`, that
//! wraps them into the module table and returns it. Both are Astra's, so `load` does no more
//! than Astra's own `register_components` and `require` do between them, once per module and
//! only when the module is first required.

use bitflags::bitflags;
use mlua::{Lua, Value};

use crate::components::{astra_serde, crypto, datetime, file_system, http, utils};

/// One of the stdlib modules.
///
/// Each knows the name Lua `require`s it by. The set of modules is this enum; what a module
/// contains is this crate's business and never the core's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum StdModule {
    /// An HTTP client.
    Http,
    /// Files and directories.
    Fs,
    /// Hashing, HMAC, base64 and UUIDs.
    Crypto,
    /// JSON, JSON5, YAML, TOML, INI, CSV and XML encoding and decoding.
    Serde,
    /// Instants, civil dates and times, zones and spans.
    Datetime,
    /// Tasks, regular expressions and `env.get`.
    Utils,
    /// In-memory key/value, observable and pubsub stores.
    Stores,
}

impl StdModule {
    /// Every module, in the order [`StdModules::modules`] yields them.
    pub const ALL: [StdModule; 7] = [
        StdModule::Http,
        StdModule::Fs,
        StdModule::Crypto,
        StdModule::Serde,
        StdModule::Datetime,
        StdModule::Utils,
        StdModule::Stores,
    ];

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
        }
    }
}

bitflags! {
    /// A set of [`StdModule`]s: which of them a runtime registers.
    ///
    /// One flag per module, named for the module. [`StdModules::ALL`] and [`StdModules::NONE`]
    /// are the two a profile starts from.
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
    }
}

impl StdModules {
    /// Every stdlib module.
    pub const ALL: StdModules = StdModules::all();

    /// No stdlib module.
    pub const NONE: StdModules = StdModules::empty();

    /// The modules in this set, in a fixed order.
    pub fn modules(self) -> impl Iterator<Item = StdModule> {
        StdModule::ALL
            .into_iter()
            .filter(move |module| self.contains(StdModules::from(*module)))
    }
}

impl From<StdModule> for StdModules {
    fn from(module: StdModule) -> Self {
        match module {
            StdModule::Http => StdModules::HTTP,
            StdModule::Fs => StdModules::FS,
            StdModule::Crypto => StdModules::CRYPTO,
            StdModule::Serde => StdModules::SERDE,
            StdModule::Datetime => StdModules::DATETIME,
            StdModule::Utils => StdModules::UTILS,
            StdModule::Stores => StdModules::STORES,
        }
    }
}

impl FromIterator<StdModule> for StdModules {
    fn from_iter<I: IntoIterator<Item = StdModule>>(modules: I) -> Self {
        modules.into_iter().fold(StdModules::NONE, |set, module| {
            set | StdModules::from(module)
        })
    }
}

/// Builds `module`'s value: registers its primitives, then runs its Lua layer.
pub(crate) fn load(lua: &Lua, module: StdModule) -> mlua::Result<Value> {
    let source = match module {
        StdModule::Http => {
            http::client::HTTPClientRequest::register_to_lua(lua)?;
            include_str!("../lua/http.lua")
        }
        StdModule::Fs => {
            file_system::register_to_lua(lua)?;
            file_system::GlobResult::register_to_lua(lua)?;
            include_str!("../lua/fs.lua")
        }
        StdModule::Crypto => {
            crypto::register_to_lua(lua)?;
            include_str!("../lua/crypto.lua")
        }
        StdModule::Serde => {
            astra_serde::register_to_lua(lua)?;
            include_str!("../lua/serde.lua")
        }
        StdModule::Datetime => {
            datetime::AstraDateTime::register_to_lua(lua)?;
            include_str!("../lua/datetime.lua")
        }
        StdModule::Utils => {
            utils::register_to_lua(lua)?;
            include_str!("../lua/utils.lua")
        }
        // Astra's `stores` has no Rust half: it is Lua all the way down.
        StdModule::Stores => include_str!("../lua/stores.lua"),
    };
    // Named so a traceback through a stdlib module says where it came from. Text only: the
    // sources are embedded, and a chunk that is not text is not one of ours.
    lua.load(source)
        .set_name(format!("=[avarice-rt stdlib {}]", module.name()))
        .set_mode(mlua::chunk::ChunkMode::Text)
        .eval()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_what_lua_requires_them_by() {
        let names: Vec<_> = StdModule::ALL.into_iter().map(StdModule::name).collect();
        assert_eq!(
            names,
            [
                "http", "fs", "crypto", "serde", "datetime", "utils", "stores"
            ]
        );
    }

    #[test]
    fn all_is_every_module_and_none_is_no_module() {
        assert_eq!(StdModules::ALL.modules().count(), 7);
        assert_eq!(StdModules::NONE.modules().count(), 0);
        for module in StdModule::ALL {
            assert!(StdModules::ALL.contains(module.into()));
            assert!(!StdModules::NONE.contains(module.into()));
        }
    }

    #[test]
    fn every_module_is_exactly_one_flag() {
        // The flag set and the enum cannot drift apart: each variant maps to a distinct single
        // flag, and all of them together are `ALL`.
        let mut union = StdModules::NONE;
        for module in StdModule::ALL {
            let flag = StdModules::from(module);
            assert_eq!(flag.bits().count_ones(), 1, "{module:?}");
            assert!(!union.intersects(flag), "{module:?} shares a bit");
            union |= flag;
        }
        assert_eq!(union, StdModules::ALL);
    }

    #[test]
    fn modules_yields_the_set_variants_in_a_fixed_order() {
        let set = StdModules::STORES | StdModules::HTTP | StdModules::CRYPTO;
        let modules: Vec<_> = set.modules().collect();
        assert_eq!(
            modules,
            [StdModule::Http, StdModule::Crypto, StdModule::Stores]
        );
    }

    #[test]
    fn set_algebra_adds_and_subtracts() {
        let without_http = StdModules::ALL - StdModules::HTTP;
        assert!(!without_http.contains(StdModules::HTTP));
        assert_eq!(without_http.modules().count(), 6);
        assert_eq!(without_http | StdModules::HTTP, StdModules::ALL);
        assert_eq!(StdModules::ALL & !StdModules::ALL, StdModules::NONE);
    }

    #[test]
    fn a_set_collects_from_modules() {
        let set: StdModules = [StdModule::Fs, StdModule::Utils].into_iter().collect();
        assert_eq!(set, StdModules::FS | StdModules::UTILS);
    }
}
