//! What the sandbox profile does and does not hand to Lua.

use avarice_rt::{
    DEFAULT_SANDBOX_MEMORY_LIMIT, Profile, Runtime, StdModule, StdModules, was_out_of_memory,
};

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
    assert!(
        rt.block_on(rt.eval::<i64>(bytecode.clone(), "=payload"))
            .is_err()
    );
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

/// Runs `setmetatable(t, mt)` with `mt` as given in Lua, and gives back its error, if any.
fn setmetatable_error(rt: &Runtime, mt: &str) -> Option<String> {
    let source = format!("setmetatable({{}}, {mt})");
    rt.block_on(rt.exec(&source, "=test")).err().map(|e| e.to_string())
}

/// Whether a table whose metatable has a finalizer has it run once the table is collected.
fn runs_a_finalizer(rt: &Runtime) -> bool {
    rt.block_on(rt.eval(
        r#"
        local ran = false
        setmetatable({}, { __gc = function() ran = true end })
        collectgarbage()
        collectgarbage()
        return ran
        "#,
        "=test",
    ))
    .unwrap()
}

#[test]
fn refuses_a_lua_finalizer_where_the_script_set_it() {
    // A finalizer runs with hooks off, so neither a time limit nor a cancel could stop one.
    let message = setmetatable_error(&sandbox(), "{ __gc = function() end }")
        .expect("a finalizer should be refused");
    assert!(
        message.contains("test:1: setmetatable: finalizers (__gc) are not allowed in this runtime"),
        "{message}"
    );
}

#[test]
fn refuses_a_placeholder_that_would_mark_the_table_for_a_finalizer_set_later() {
    // Lua marks a table for finalizing if its metatable has any `__gc` when it is set, and
    // calls whatever function is there by the time the table is collected.
    for placeholder in ["true", "false", "0"] {
        assert!(
            setmetatable_error(&sandbox(), &format!("{{ __gc = {placeholder} }}")).is_some(),
            "__gc = {placeholder} should be refused"
        );
    }
}

#[test]
fn still_sets_metatables_without_a_finalizer() {
    let rt = sandbox();
    let ok: bool = rt
        .block_on(rt.eval(
            r#"
            local mt = { __index = { x = 1 } }
            local t = setmetatable({}, mt)
            return t.x == 1 and getmetatable(t) == mt and setmetatable(t, nil) == t
            "#,
            "=test",
        ))
        .unwrap();
    assert!(ok);
    // An `__index` that could answer `__gc` does not count: Lua looks for it raw, and so do we.
    assert_eq!(
        setmetatable_error(&rt, "{ __index = { __gc = true } }"),
        None
    );
}

#[test]
fn a_bad_argument_to_setmetatable_is_reported_in_lua_s_words_at_the_script_s_line() {
    let rt = sandbox();
    for (call, expected) in [
        ("setmetatable(1, {})", "bad argument #1 to 'setmetatable' (table expected, got number)"),
        (
            "setmetatable({}, 1)",
            "bad argument #2 to 'setmetatable' (nil or table expected, got number)",
        ),
        (
            "setmetatable(setmetatable({}, { __metatable = 1 }), {})",
            "cannot change a protected metatable",
        ),
    ] {
        let err = rt.block_on(rt.exec(call, "=test")).unwrap_err().to_string();
        assert!(err.contains(&format!("test:1: {expected}")), "{call}: {err}");
    }
}

#[test]
fn replacing_the_setmetatable_global_does_not_reach_past_the_refusal() {
    let rt = sandbox();
    let refused: bool = rt
        .block_on(rt.eval(
            r#"
            local saved = setmetatable
            setmetatable = nil
            _G.setmetatable = function(t, mt) return saved(t, mt) end
            return not pcall(setmetatable, {}, { __gc = function() end })
                and not pcall(saved, {}, { __gc = function() end })
            "#,
            "=test",
        ))
        .unwrap();
    assert!(refused);
    assert!(setmetatable_error(&rt, "{ __gc = function() end }").is_some());
}

#[test]
fn replacing_the_globals_the_refusal_uses_does_not_reach_past_it() {
    let rt = sandbox();
    let refused: bool = rt
        .block_on(rt.eval(
            r#"
            local mt = { __gc = function() end }
            rawget = function() return nil end
            type = function() return "number" end
            string.gsub = function() return "" end
            return not pcall(setmetatable, {}, mt)
            "#,
            "=test",
        ))
        .unwrap();
    assert!(refused);
}

#[test]
fn trusted_runs_lua_finalizers() {
    assert!(runs_a_finalizer(&Runtime::new(Profile::Trusted).unwrap()));
}

#[test]
fn lua_finalizers_can_be_allowed_in_the_sandbox_and_refused_in_trusted() {
    let allowed = Runtime::builder(Profile::Sandbox)
        .allow_lua_finalizers(true)
        .build()
        .unwrap();
    assert!(runs_a_finalizer(&allowed));

    let refused = Runtime::builder(Profile::Trusted)
        .allow_lua_finalizers(false)
        .build()
        .unwrap();
    assert!(setmetatable_error(&refused, "{ __gc = function() end }").is_some());
}

#[test]
fn only_trusted_allows_lua_finalizers_by_default() {
    assert!(!Profile::Sandbox.allows_lua_finalizers());
    assert!(Profile::Trusted.allows_lua_finalizers());
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
fn reaches_no_stdlib_module_that_is_not_pure_and_so_neither_the_network_nor_the_filesystem() {
    let rt = sandbox();
    // Every module that is not pure, including any this build does not have: none may be
    // reachable.
    for name in (StdModules::all() - StdModules::PURE)
        .modules()
        .map(StdModule::name)
    {
        let absent = rt
            .block_on(rt.eval::<bool>(
                &format!("local ok = pcall(require, '{name}') return not ok"),
                "=test",
            ))
            .unwrap();
        assert!(absent, "{name} should not be reachable");
    }
    // Nor have their primitives leaked into the state without a module to carry them.
    let leaked = rt
        .block_on(rt.eval::<bool>(
            "for k in pairs(_G) do \
                 if tostring(k):find('^astra_internal__') then return true end \
             end \
             return false",
            "=test",
        ))
        .unwrap();
    assert!(!leaked, "a stdlib primitive is present in a sandbox");
}

#[test]
fn reaches_the_pure_stdlib_modules_and_they_have_no_rust_behind_them() {
    let rt = sandbox();
    for name in StdModules::PURE.modules().map(StdModule::name) {
        let built = rt
            .block_on(rt.eval::<bool>(&format!("return pcall(require, '{name}')"), "=test"))
            .unwrap();
        assert!(built, "{name} is pure, so a sandbox should have it");
    }
    // A module with Rust behind it sets its primitives as globals when it is built. A pure
    // module has none to set, and that is what puts it inside the memory cap and the time limit
    // (ADR 0007), so building every pure module must have added no primitive to the state.
    let primitives = rt
        .block_on(rt.eval::<bool>(
            "for k in pairs(_G) do \
                 if tostring(k):find('^astra_internal__') then return true end \
             end \
             return false",
            "=test",
        ))
        .unwrap();
    assert!(!primitives, "a pure module has a Rust half");
}
