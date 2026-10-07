//! `greeting`, a module contributed to avarice from outside it (ADR 0020).
//!
//! A contributed module is a type that implements [`HostModule`]. This one has a Rust half, which
//! the module's configuration reaches, and a Lua half, which builds on it:
//!
//! ```lua
//! local greeting = require("greeting")
//! print(greeting.hello("world"))           --> Hello, world!
//! print(greeting.hello_all({ "Ada", "Grace" }))
//! ```

use avarice::HostModule;
use avarice::mlua::{self, Lua, Table, Value};

/// The `greeting` module, greeting with a salutation the embedder chooses.
pub struct Greeting {
    salutation: String,
}

impl Greeting {
    /// A `greeting` that greets with `salutation`, as in "Hello, world!".
    pub fn new(salutation: impl Into<String>) -> Self {
        Greeting {
            salutation: salutation.into(),
        }
    }
}

impl HostModule for Greeting {
    fn name(&self) -> &str {
        "greeting"
    }

    fn load(&self, lua: &Lua) -> mlua::Result<Value> {
        // The Rust half. A runtime builds the module once, but one `Greeting` can serve several
        // runtimes, so each function gets its own copy of the salutation.
        let module = lua.create_table()?;
        let salutation = self.salutation.clone();
        module.set(
            "hello",
            lua.create_function(move |_, name: String| Ok(format!("{salutation}, {name}!")))?,
        )?;

        // The Lua half, given the table the Rust half built.
        let module: Table = lua
            .load(include_str!("greeting.lua"))
            .set_name("@greeting.lua")
            .call(module)?;
        Ok(Value::Table(module))
    }
}

#[cfg(test)]
mod tests {
    use avarice::{Profile, Runtime};

    use super::*;

    #[test]
    fn greets_with_the_salutation_it_was_given() {
        let rt = Runtime::builder(Profile::Sandbox)
            .module(Greeting::new("Ahoy"))
            .build()
            .unwrap();
        let out: String = rt
            .block_on(rt.eval(
                "return require('greeting').hello_all({ 'a', 'b' })",
                "=test",
            ))
            .unwrap();
        assert_eq!(out, "Ahoy, a!\nAhoy, b!");
    }
}
