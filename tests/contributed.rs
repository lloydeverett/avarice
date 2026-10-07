//! Contributed modules: host modules another crate defines, registered through the builder.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use avarice::mlua::{self, Lua, Value};
use avarice::{Error, HostModule, Profile, Runtime, StdModules};

/// A module that counts how often it is built, standing in for one from another crate.
struct Counted {
    name: &'static str,
    loads: Arc<AtomicUsize>,
}

impl Counted {
    fn new(name: &'static str) -> Self {
        Counted {
            name,
            loads: Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl HostModule for Counted {
    fn name(&self) -> &str {
        self.name
    }

    fn load(&self, lua: &Lua) -> mlua::Result<Value> {
        self.loads.fetch_add(1, Ordering::Relaxed);
        let module = lua.create_table()?;
        module.set("name", self.name)?;
        Ok(Value::Table(module))
    }
}

/// The message of a refused build, which must be a configuration error.
fn refusal(result: avarice::Result<Runtime>) -> String {
    match result {
        Err(Error::Config(msg)) => msg,
        Err(other) => panic!("expected a configuration error, got {other:?}"),
        Ok(_) => panic!("expected the build to be refused"),
    }
}

#[test]
fn a_contributed_module_is_required_by_its_name() {
    for profile in [Profile::Sandbox, Profile::Trusted] {
        let rt = Runtime::builder(profile)
            .module(Counted::new("foo.bar"))
            .build()
            .unwrap();
        assert!(rt.has_module("foo.bar"));
        let name: String = rt
            .block_on(rt.eval("return require('foo.bar').name", "=test"))
            .unwrap();
        assert_eq!(name, "foo.bar");
    }
}

#[test]
fn a_contributed_module_is_built_once_and_only_when_required() {
    let module = Counted::new("foo");
    let loads = Arc::clone(&module.loads);
    let rt = Runtime::builder(Profile::Sandbox)
        .module(module)
        .build()
        .unwrap();
    assert_eq!(
        loads.load(Ordering::Relaxed),
        0,
        "built before it was required"
    );

    let same: bool = rt
        .block_on(rt.eval("return require('foo') == require('foo')", "=test"))
        .unwrap();
    assert!(same);
    assert_eq!(loads.load(Ordering::Relaxed), 1, "built more than once");
}

#[test]
fn every_runtime_built_from_one_builder_gets_the_module() {
    let module = Counted::new("foo");
    let loads = Arc::clone(&module.loads);
    let builder = Runtime::builder(Profile::Sandbox).module(module);
    for rt in [builder.clone().build().unwrap(), builder.build().unwrap()] {
        rt.block_on(rt.exec("require('foo')", "=test")).unwrap();
    }
    assert_eq!(
        loads.load(Ordering::Relaxed),
        2,
        "each runtime builds its own"
    );
}

#[test]
fn an_error_from_the_loader_is_raised_by_require() {
    struct Broken;
    impl HostModule for Broken {
        fn name(&self) -> &str {
            "broken"
        }
        fn load(&self, _: &Lua) -> mlua::Result<Value> {
            Err(mlua::Error::runtime("could not build broken"))
        }
    }

    let rt = Runtime::builder(Profile::Sandbox)
        .module(Broken)
        .build()
        .unwrap();
    let err = rt
        .block_on(rt.exec("require('broken')", "=test"))
        .unwrap_err();
    assert!(err.to_string().contains("could not build broken"), "{err}");
}

#[test]
fn an_invalid_name_is_refused_at_build() {
    let result = Runtime::builder(Profile::Sandbox)
        .module(Counted::new("../etc"))
        .build();
    assert!(matches!(result, Err(Error::ModuleName(_))), "{result:?}");
}

#[test]
fn two_contributed_modules_of_one_name_are_refused() {
    let msg = refusal(
        Runtime::builder(Profile::Sandbox)
            .module(Counted::new("foo"))
            .module(Counted::new("foo"))
            .build(),
    );
    assert!(msg.contains("'foo'"), "{msg}");
}

#[test]
fn a_contributed_module_cannot_take_the_core_module_s_name() {
    let msg = refusal(
        Runtime::builder(Profile::Sandbox)
            .module(Counted::new("ansi"))
            .build(),
    );
    assert!(msg.contains("'ansi'"), "{msg}");
}

#[test]
fn a_contributed_module_cannot_take_a_standard_library_s_name() {
    let msg = refusal(
        Runtime::builder(Profile::Sandbox)
            .module(Counted::new("string"))
            .build(),
    );
    assert!(msg.contains("'string'"), "{msg}");
}

#[cfg(feature = "stdlib-stores")]
#[test]
fn a_contributed_module_cannot_take_a_selected_stdlib_module_s_name() {
    let msg = refusal(
        Runtime::builder(Profile::Sandbox)
            .with_std_modules(StdModules::STORES)
            .module(Counted::new("stores"))
            .build(),
    );
    assert!(msg.contains("'stores'"), "{msg}");
}

#[cfg(feature = "stdlib-fs")]
#[test]
fn a_contributed_module_cannot_take_the_name_of_a_module_its_profile_registers() {
    let msg = refusal(
        Runtime::builder(Profile::Trusted)
            .module(Counted::new("fs"))
            .build(),
    );
    assert!(msg.contains("'fs'"), "{msg}");
}

#[test]
fn a_contributed_module_may_take_an_unregistered_stdlib_module_s_name() {
    let rt = Runtime::builder(Profile::Sandbox)
        .std_modules(StdModules::empty())
        .module(Counted::new("fs"))
        .build()
        .unwrap();
    let name: String = rt
        .block_on(rt.eval("return require('fs').name", "=test"))
        .unwrap();
    assert_eq!(name, "fs");
}

#[test]
fn the_stdlib_list_leaves_contributed_modules_out() {
    let rt = Runtime::builder(Profile::Sandbox)
        .std_modules(StdModules::empty())
        .module(Counted::new("foo"))
        .build()
        .unwrap();
    let count: i64 = rt.block_on(rt.eval("return #stdlib()", "=test")).unwrap();
    assert_eq!(count, 0);
}
