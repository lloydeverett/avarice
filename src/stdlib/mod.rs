//! The avarice-rt stdlib modules, derived from [Astra](https://github.com/ArkForgeLabs/Astra).
//!
//! This directory holds the code derived from Astra. It is self-contained, and the dependency runs
//! one way: it names [`mlua`], [`tokio`], `bitflags` and its own optional dependencies, and nothing
//! else in `avarice-rt`. That is a convention, not something the compiler checks (ADR 0012).
//!
//! The whole surface between this directory and the rest of the crate is three things:
//! [`StdModule`], one variant per module, each knowing the name it is `require`d by;
//! [`StdModules`], a set of them; and [`loader`], which builds a module's value on demand. The
//! core registers each selected module's loader as an ordinary lazy module and never learns what
//! any of them contains.
//!
//! Every Astra file opens with a header naming its origin and what was changed; the root
//! `README.md` says how they are kept.

pub(crate) mod components;
mod modules;

#[cfg(test)]
mod compiled_in_tests;

use mlua::{Lua, Value};

pub use self::modules::{StdModule, StdModules};

/// The loader for `module`: a function that builds the module's value in a Lua state.
///
/// Built on demand, so nothing is constructed for a module nobody requires. The core hands this
/// to its lazy-module registration, which is the same path an embedder's own lazy module takes.
pub fn loader(module: StdModule) -> impl Fn(&Lua) -> mlua::Result<Value> + 'static {
    move |lua| modules::load(lua, module)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The core hands the loader to Lua's `require`, which under mlua's `send` feature must be
    /// `Send`, and shares it behind an `Arc`. That holds only because an opaque return type leaks
    /// its auto traits, so it is asserted here rather than left to a change that quietly breaks it.
    #[test]
    fn a_loader_is_send_and_sync() {
        fn check(_: impl Send + Sync + 'static) {}
        check(loader(StdModule::Stores));
    }
}
