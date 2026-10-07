//! An embedder: a program that builds a Lua runtime with avarice, adds a module another crate
//! contributed, and runs a Lua program in it.

use std::time::Duration;

use avarice::{Error, FsStore, Profile, Runtime, StdModules, was_out_of_memory};
use avarice_greeting::Greeting;

fn main() -> avarice::Result<()> {
    // Start from a profile, here the sandbox: no `io`, `os`, binary chunks or `__gc`, and only the
    // pure stdlib modules. Everything after it overrides one of its defaults, or adds to them.
    let rt = Runtime::builder(Profile::Sandbox)
        // A stdlib module, compiled in by this crate's `stdlib-crypto` feature.
        .with_std_modules(StdModules::CRYPTO)
        // A module another crate contributes.
        .module(Greeting::new("Hello"))
        // Lua may allocate 16 MiB, and run for 5 seconds per execution.
        .memory_limit(16 * 1024 * 1024)
        .time_limit(Duration::from_secs(5))
        // Where `require` looks once no host module matches: `require("text")` is lua/text.lua.
        .store(FsStore::new(concat!(env!("CARGO_MANIFEST_DIR"), "/lua")))
        .build()?;

    // A host module of the app's own, registered on the built runtime.
    let config = rt.lua().create_table()?;
    config.set("user", "world")?;
    rt.register_module("config", config)?;

    // Run the program. Lua runs as a future on the runtime's executor, which `block_on` drives.
    rt.block_on(rt.exec("require('main')", "=app"))?;

    // Hand a value back to Rust.
    let digest: String = rt.block_on(rt.eval(
        "return require('crypto').hash('sha2_256', 'avarice')",
        "=app",
    ))?;
    println!("digest: {digest}");

    // A limit tripping is an error from Rust's side, which the app can tell apart from others.
    match rt.block_on(rt.exec("local s = string.rep('x', 64 * 1024 * 1024)", "=greedy")) {
        Err(Error::Lua(e)) if was_out_of_memory(&e) => println!("refused: out of memory"),
        other => other?,
    }
    Ok(())
}
