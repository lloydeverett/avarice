//! The avarice-rt stdlib modules, derived from [Astra](https://github.com/ArkForgeLabs/Astra).
//!
//! This crate is the Apache-2.0 boundary of the repository: code derived from Astra, and the
//! licence obligations that come with it, live here and nowhere else. It depends on [`mlua`] and
//! never on `avarice-rt`.
//!
//! The whole surface between this crate and the core is three things: [`StdModule`], one variant
//! per module, each knowing the name it is `require`d by; [`StdModules`], a set of them; and
//! [`loader`], which builds a module's value on demand. The core registers each selected
//! module's loader as an ordinary lazy module and never learns what any of them contains.
//!
//! See the crate's `README.md` and `UPSTREAM.md` for how the Astra sources are kept and marked.

mod components;
mod modules;

use mlua::{Lua, Value};

pub use crate::modules::{StdModule, StdModules};

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
