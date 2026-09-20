//! What `require` resolves, and what it refuses to.

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use avarice_rt::mlua::{Table, Value};
use avarice_rt::{
    Error, FsStore, ModuleName, ModuleSource, ModuleStore, Profile, Runtime, StoreError,
};
use common::TempDir;

#[test]
fn resolves_a_host_registered_module() {
    let rt = Runtime::new(Profile::Sandbox).unwrap();
    let greeting = rt.lua().create_table().unwrap();
    greeting
        .set(
            "hello",
            rt.lua()
                .create_function(|_, name: String| Ok(format!("hello, {name}")))
                .unwrap(),
        )
        .unwrap();
    rt.register_module("greeting", greeting).unwrap();

    assert!(rt.has_module("greeting"));
    let out: String = rt
        .eval("return require('greeting').hello('world')", "=test")
        .unwrap();
    assert_eq!(out, "hello, world");
}

#[test]
fn a_lazy_module_loads_once_and_only_when_asked() {
    let rt = Runtime::new(Profile::Sandbox).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    rt.register_lazy_module("counted", move |lua| {
        counter.fetch_add(1, Ordering::Relaxed);
        Ok(Value::Integer(lua.create_table()?.len()? + 41))
    })
    .unwrap();

    assert_eq!(
        calls.load(Ordering::Relaxed),
        0,
        "loader ran before it was required"
    );
    let value: i64 = rt
        .eval(
            "local a = require('counted') local b = require('counted') return a + b",
            "=test",
        )
        .unwrap();
    assert_eq!(value, 82);
    assert_eq!(
        calls.load(Ordering::Relaxed),
        1,
        "loader ran more than once"
    );
}

#[test]
fn require_hands_back_the_same_value_every_time() {
    let rt = Runtime::new(Profile::Sandbox).unwrap();
    rt.register_module("thing", rt.lua().create_table().unwrap())
        .unwrap();
    let same: bool = rt
        .eval("return require('thing') == require('thing')", "=test")
        .unwrap();
    assert!(same);
}

#[test]
fn require_reaches_the_standard_libraries_lua_already_loaded() {
    let rt = Runtime::new(Profile::Sandbox).unwrap();
    let same: bool = rt
        .eval("return require('string') == string", "=test")
        .unwrap();
    assert!(same);
}

#[test]
fn resolves_dotted_names_from_the_filesystem_store() {
    let dir = TempDir::new();
    dir.write(
        "app/util.lua",
        "return { double = function(n) return n * 2 end }",
    );
    dir.write("app/init.lua", "return { name = 'app' }");

    let rt = Runtime::builder(Profile::Sandbox)
        .store(FsStore::new(dir.path()))
        .build()
        .unwrap();

    let doubled: i64 = rt
        .eval("return require('app.util').double(21)", "=test")
        .unwrap();
    assert_eq!(doubled, 42);

    // A directory's `init.lua` stands in for the directory itself.
    let name: String = rt.eval("return require('app').name", "=test").unwrap();
    assert_eq!(name, "app");
}

#[test]
fn a_module_is_told_its_own_name() {
    let dir = TempDir::new();
    dir.write("echo.lua", "local name = ... return name");
    let rt = Runtime::builder(Profile::Sandbox)
        .store(FsStore::new(dir.path()))
        .build()
        .unwrap();
    let name: String = rt.eval("return require('echo')", "=test").unwrap();
    assert_eq!(name, "echo");
}

#[test]
fn a_module_that_returns_nothing_is_recorded_as_true() {
    let dir = TempDir::new();
    dir.write("silent.lua", "local x = 1");
    let rt = Runtime::builder(Profile::Sandbox)
        .store(FsStore::new(dir.path()))
        .build()
        .unwrap();
    let value: bool = rt.eval("return require('silent')", "=test").unwrap();
    assert!(value);
}

#[test]
fn errors_in_a_module_reach_the_caller() {
    let dir = TempDir::new();
    dir.write("bad.lua", "error('module exploded')");
    let rt = Runtime::builder(Profile::Sandbox)
        .store(FsStore::new(dir.path()))
        .build()
        .unwrap();
    let err = rt.exec("require('bad')", "=test").unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("module exploded"), "{msg}");
    assert!(msg.contains("bad.lua"), "{msg}");
}

#[test]
fn a_require_cycle_is_reported_rather_than_recursing() {
    let dir = TempDir::new();
    dir.write("a.lua", "require('b') return {}");
    dir.write("b.lua", "require('a') return {}");
    let rt = Runtime::builder(Profile::Sandbox)
        .store(FsStore::new(dir.path()))
        .build()
        .unwrap();

    let err = rt.exec("require('a')", "=test").unwrap_err().to_string();
    assert!(err.contains("cycle"), "{err}");
    assert!(err.contains("a -> b -> a"), "{err}");
}

#[test]
fn a_failed_module_can_be_required_again() {
    let dir = TempDir::new();
    dir.write("flaky.lua", "error('not yet')");
    let rt = Runtime::builder(Profile::Sandbox)
        .store(FsStore::new(dir.path()))
        .build()
        .unwrap();

    assert!(rt.exec("require('flaky')", "=test").is_err());
    dir.write("flaky.lua", "return 'fixed'");
    let value: String = rt.eval("return require('flaky')", "=test").unwrap();
    assert_eq!(value, "fixed");
}

#[test]
fn missing_modules_say_where_we_looked() {
    let rt = Runtime::new(Profile::Sandbox).unwrap();
    let err = rt.exec("require('nope')", "=test").unwrap_err().to_string();
    assert!(err.contains("module 'nope' not found"), "{err}");
    assert!(err.contains("no module store is configured"), "{err}");

    let dir = TempDir::new();
    let rt = Runtime::builder(Profile::Sandbox)
        .store(FsStore::new(dir.path()))
        .build()
        .unwrap();
    let err = rt.exec("require('nope')", "=test").unwrap_err().to_string();
    assert!(err.contains("filesystem store rooted at"), "{err}");
}

#[test]
fn a_module_name_can_never_address_a_file_outside_the_store() {
    let dir = TempDir::new();
    let rt = Runtime::builder(Profile::Sandbox)
        .store(FsStore::new(dir.path()))
        .build()
        .unwrap();

    for attempt in ["../../etc/passwd", "/etc/passwd", "a/../../b", "..", "a b"] {
        let err = rt
            .exec(format!("require([[{attempt}]])"), "=test")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("invalid module name"),
            "{attempt:?} produced {err}"
        );
    }
}

#[test]
fn registering_an_ill_formed_name_is_refused_in_rust_too() {
    let rt = Runtime::new(Profile::Sandbox).unwrap();
    let err = rt
        .register_module("../evil", rt.lua().create_table().unwrap())
        .unwrap_err();
    assert!(matches!(err, Error::ModuleName(_)), "{err:?}");
    assert!(!rt.has_module("../evil"));
}

#[test]
fn a_store_failure_is_not_mistaken_for_a_missing_module() {
    struct BrokenStore;
    impl ModuleStore for BrokenStore {
        fn fetch(&self, _: &ModuleName) -> Result<Option<ModuleSource>, StoreError> {
            Err(StoreError::other("the database is on fire"))
        }
    }

    let rt = Runtime::builder(Profile::Sandbox)
        .store(BrokenStore)
        .build()
        .unwrap();
    let err = rt.exec("require('any')", "=test").unwrap_err().to_string();
    assert!(err.contains("could not be read"), "{err}");
    assert!(err.contains("the database is on fire"), "{err}");
}

#[test]
fn a_store_need_not_be_a_filesystem() {
    struct InMemory;
    impl ModuleStore for InMemory {
        fn fetch(&self, name: &ModuleName) -> Result<Option<ModuleSource>, StoreError> {
            match name.as_str() {
                "answers" => Ok(Some(ModuleSource::new(
                    "return { everything = 42 }",
                    "memory:answers",
                ))),
                _ => Ok(None),
            }
        }

        fn describe(&self) -> String {
            "the in-memory store".to_string()
        }
    }

    let rt = Runtime::builder(Profile::Sandbox)
        .store(InMemory)
        .build()
        .unwrap();
    let answer: i64 = rt
        .eval("return require('answers').everything", "=test")
        .unwrap();
    assert_eq!(answer, 42);

    let err = rt
        .exec("require('other')", "=test")
        .unwrap_err()
        .to_string();
    assert!(err.contains("the in-memory store"), "{err}");
}

#[test]
fn the_store_can_be_replaced_after_the_runtime_is_built() {
    let dir = TempDir::new();
    dir.write("late.lua", "return 'late'");

    let rt = Runtime::new(Profile::Sandbox).unwrap();
    assert!(rt.exec("require('late')", "=test").is_err());

    rt.set_store(FsStore::new(dir.path()));
    let value: String = rt.eval("return require('late')", "=test").unwrap();
    assert_eq!(value, "late");

    rt.clear_store();
    // Already loaded, so still resolvable from the cache.
    assert!(rt.exec("require('late')", "=test").is_ok());
    assert!(rt.exec("require('other')", "=test").is_err());
}

#[test]
fn a_host_module_wins_over_the_store() {
    let dir = TempDir::new();
    dir.write("conflict.lua", "return 'from the store'");
    let rt = Runtime::builder(Profile::Sandbox)
        .store(FsStore::new(dir.path()))
        .build()
        .unwrap();
    rt.register_module("conflict", "from the host").unwrap();

    let value: String = rt.eval("return require('conflict')", "=test").unwrap();
    assert_eq!(value, "from the host");
}

#[test]
fn lua_cannot_reach_the_filesystem_through_the_module_system() {
    let rt = Runtime::new(Profile::Sandbox).unwrap();
    // No `package` table means no `package.path`, no `package.loadlib`, and no searchers
    // to append one of our own to.
    let globals: Table = rt.lua().globals();
    assert!(globals.get::<Value>("package").unwrap().is_nil());
    let loaded_is_hidden: bool = rt
        .eval("return _LOADED == nil and _G._LOADED == nil", "=test")
        .unwrap();
    assert!(loaded_is_hidden, "the module cache should not be a global");
}

#[test]
fn a_runtime_is_send_and_sync() {
    // Follows from mlua's `send` feature, which the stdlib crate needs (ADR 0004's amendment).
    // Asserted so that a field which is not thread-safe is caught when it is added.
    fn check<T: Send + Sync>() {}
    check::<Runtime>();
}
