//! What the sandbox profile does and does not hand to Lua.

use avarice_rt::{was_out_of_memory, Profile, Runtime, DEFAULT_SANDBOX_MEMORY_LIMIT};

fn sandbox() -> Runtime {
    Runtime::new(Profile::Sandbox).expect("sandbox runtime")
}

fn is_nil(rt: &Runtime, expr: &str) -> bool {
    rt.block_on(rt.eval::<bool>(&format!("return ({expr}) == nil"), "=test"))
        .unwrap()
}

#[test]
fn withholds_the_libraries_that_reach_outside() {
    let rt = sandbox();
    for global in ["io", "os", "package", "require('package')"] {
        // `package` is absent as a global and unreachable through require.
        let absent = rt
            .block_on(rt.eval::<bool>(
                &format!(
                    "local ok, v = pcall(function() return {global} end) return not ok or v == nil"
                ),
                "=test",
            ))
            .unwrap();
        assert!(absent, "{global} should not be reachable");
    }
}

#[test]
fn withholds_the_file_functions_hiding_in_the_base_library() {
    let rt = sandbox();
    assert!(is_nil(&rt, "dofile"), "dofile should be gone");
    assert!(is_nil(&rt, "loadfile"), "loadfile should be gone");
}

#[test]
fn keeps_the_libraries_a_script_needs() {
    let rt = sandbox();
    assert!(!is_nil(&rt, "string.format"));
    assert!(!is_nil(&rt, "table.concat"));
    assert!(!is_nil(&rt, "math.floor"));
    assert!(!is_nil(&rt, "utf8.char"));
    assert!(!is_nil(&rt, "coroutine.create"));
    assert!(!is_nil(&rt, "pcall"));
}

#[test]
fn offers_traceback_but_not_the_rest_of_debug() {
    let rt = sandbox();
    let traceback: String = rt
        .block_on(rt.eval(
            r#"
            local ok, tb = xpcall(function() error("boom") end, debug.traceback)
            return tb
            "#,
            "=test",
        ))
        .unwrap();
    assert!(traceback.contains("boom"), "{traceback}");
    assert!(traceback.contains("stack traceback"), "{traceback}");
    assert!(is_nil(&rt, "debug.getinfo"), "debug.getinfo should be gone");
    assert!(
        is_nil(&rt, "debug.setmetatable"),
        "debug.setmetatable should be gone"
    );
}

#[test]
fn refuses_binary_chunks() {
    let rt = sandbox();
    // `load` with an explicit binary mode fails rather than being quietly honoured.
    let err: String = rt
        .block_on(rt.eval(
            r#"
            local chunk = string.dump(function() return 1 end)
            local f, err = load(chunk, "=payload", "b")
            assert(f == nil, "binary chunk was accepted")
            return err
            "#,
            "=test",
        ))
        .unwrap();
    assert!(err.contains("binary"), "{err}");

    // And the default mode, which Lua would otherwise let through, is forced to text too.
    let rejected: bool = rt
        .block_on(rt.eval(
            r#"
            local chunk = string.dump(function() return 1 end)
            return load(chunk) == nil
            "#,
            "=test",
        ))
        .unwrap();
    assert!(rejected, "binary chunk accepted through the default mode");
}

#[test]
fn refuses_binary_chunks_handed_straight_to_the_runtime() {
    // `Runtime::eval` tries the entry as an expression first; neither that path nor the
    // statement path may accept bytecode in the sandbox.
    let trusted = Runtime::new(Profile::Trusted).unwrap();
    let bytecode = trusted
        .block_on(trusted.eval::<avarice_rt::mlua::LuaString>(
            "return string.dump(function() return 7 end)",
            "=test",
        ))
        .unwrap()
        .as_bytes()
        .to_vec();
    assert_eq!(bytecode[0], 0x1b, "expected a precompiled chunk");

    let rt = sandbox();
    assert!(rt
        .block_on(rt.eval::<i64>(bytecode.clone(), "=payload"))
        .is_err());
    assert!(rt.block_on(rt.exec(bytecode, "=payload")).is_err());
}

#[test]
fn trusted_allows_binary_chunks() {
    let rt = Runtime::new(Profile::Trusted).unwrap();
    let result: i64 = rt
        .block_on(rt.eval(
            r#"
            local chunk = string.dump(function() return 7 end)
            return load(chunk, "=payload", "b")()
            "#,
            "=test",
        ))
        .unwrap();
    assert_eq!(result, 7);
}

#[test]
fn trusted_keeps_io_and_os() {
    let rt = Runtime::new(Profile::Trusted).unwrap();
    assert!(!is_nil(&rt, "io.open"));
    assert!(!is_nil(&rt, "os.time"));
    assert!(!is_nil(&rt, "loadfile"));
    // But module loading is still the host's business, not Lua's.
    assert!(is_nil(&rt, "package"));
}

#[test]
fn caps_memory_by_default() {
    let rt = sandbox();
    assert_eq!(rt.memory_limit(), Some(DEFAULT_SANDBOX_MEMORY_LIMIT));
    assert_eq!(Runtime::new(Profile::Trusted).unwrap().memory_limit(), None);
}

#[test]
fn memory_limit_stops_a_runaway_allocation() {
    let rt = Runtime::builder(Profile::Sandbox)
        .memory_limit(4 * 1024 * 1024)
        .build()
        .unwrap();
    let err = rt
        .block_on(rt.exec(
            r#"
            local t = {}
            while true do
                t[#t + 1] = string.rep("x", 4096)
            end
            "#,
            "=test",
        ))
        .unwrap_err();
    let avarice_rt::Error::Lua(err) = err else {
        panic!("expected a Lua error, got {err:?}");
    };
    assert!(was_out_of_memory(&err), "expected OOM, got {err}");
    assert!(rt.used_memory() <= 4 * 1024 * 1024);
}

#[test]
fn the_sandbox_profile_is_not_mutated_by_reconfiguring_a_runtime() {
    let loosened = Runtime::builder(Profile::Sandbox)
        .with_std_libs(avarice_rt::mlua::StdLib::OS)
        .unlimited_memory()
        .allow_binary_chunks(true)
        .build()
        .unwrap();
    assert!(!is_nil(&loosened, "os.time"));
    assert_eq!(loosened.memory_limit(), None);

    // A runtime built afterwards from the same profile is untouched.
    let fresh = sandbox();
    assert!(is_nil(&fresh, "os"));
    assert_eq!(fresh.memory_limit(), Some(DEFAULT_SANDBOX_MEMORY_LIMIT));
    assert_eq!(
        Profile::Sandbox.memory_limit(),
        Some(DEFAULT_SANDBOX_MEMORY_LIMIT)
    );
}

#[test]
fn opening_the_package_library_is_refused_rather_than_ignored() {
    // `require` is ours. Opening `package` would hand Lua back `package.loadlib` and a searcher
    // list that reaches a `.so`, which is the capability ADR 0002 exists to remove — so asking
    // for it fails loudly rather than being quietly dropped.
    for profile in [Profile::Sandbox, Profile::Trusted] {
        let err = Runtime::builder(profile)
            .with_std_libs(avarice_rt::mlua::StdLib::PACKAGE)
            .build()
            .unwrap_err();
        assert!(
            matches!(err, avarice_rt::Error::Config(ref msg) if msg.contains("package")),
            "{profile:?}: {err:?}"
        );
    }
}

#[test]
fn all_safe_is_refused_because_it_carries_package() {
    // The trap the builder's own documentation warns about: for Lua, as opposed to Luau,
    // `ALL_SAFE` includes `package`.
    let err = Runtime::builder(Profile::Sandbox)
        .std_libs(avarice_rt::mlua::StdLib::ALL_SAFE)
        .build()
        .unwrap_err();
    assert!(
        matches!(err, avarice_rt::Error::Config(ref msg) if msg.contains("package")),
        "{err:?}"
    );
}

#[test]
fn opening_the_debug_library_is_refused_rather_than_ignored() {
    let err = Runtime::builder(Profile::Trusted)
        .with_std_libs(avarice_rt::mlua::StdLib::DEBUG)
        .build()
        .unwrap_err();
    assert!(
        matches!(err, avarice_rt::Error::Config(ref msg) if msg.contains("debug")),
        "{err:?}"
    );
}

#[test]
fn reaches_no_stdlib_module_and_so_neither_the_network_nor_the_filesystem_through_one() {
    let rt = sandbox();
    for name in [
        "http", "fs", "crypto", "serde", "datetime", "utils", "stores",
    ] {
        let absent = rt
            .block_on(rt.eval::<bool>(
                &format!("local ok = pcall(require, '{name}') return not ok"),
                "=test",
            ))
            .unwrap();
        assert!(absent, "{name} should not be reachable");
    }
    // Nor have their primitives leaked into the state without a module to carry them.
    let leaked = rt.block_on(rt
        .eval::<bool>(
            "for k in pairs(_G) do if tostring(k):find('^astra_internal__') then return true end end \
             return false",
            "=test",
        ))
        .unwrap();
    assert!(!leaked, "a stdlib primitive is present in a sandbox");
}
