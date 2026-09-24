//! Time limits and cancellation, including the ways a script might try to shed them.

use std::time::{Duration, Instant};

use avarice_rt::{CancelHandle, Error, Profile, Runtime, was_cancelled, was_timed_out};

fn lua_error(err: Error) -> avarice_rt::mlua::Error {
    match err {
        Error::Lua(err) => err,
        other => panic!("expected a Lua error, got {other:?}"),
    }
}

fn timed(limit: Duration) -> Runtime {
    Runtime::builder(Profile::Sandbox)
        .time_limit(limit)
        .check_interval(1_000)
        .build()
        .unwrap()
}

#[test]
fn a_time_limit_stops_an_endless_loop() {
    let rt = timed(Duration::from_millis(100));
    let started = Instant::now();
    let err = lua_error(
        rt.block_on(rt.exec("while true do end", "=test"))
            .unwrap_err(),
    );
    assert!(was_timed_out(&err), "{err}");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "took {:?}",
        started.elapsed()
    );
}

#[test]
fn the_budget_is_per_execution_rather_than_per_runtime() {
    let rt = timed(Duration::from_millis(200));
    for _ in 0..3 {
        // Each execution starts with the whole budget, so none of these time out even though
        // together they outlast it.
        rt.block_on(rt.exec("local n = 0 for i = 1, 200000 do n = n + i end", "=test"))
            .unwrap();
    }
}

#[test]
fn a_coroutine_does_not_escape_the_limit() {
    // mlua's per-thread hooks would let this through; the global hook is inherited by every
    // coroutine, so it does not.
    let rt = timed(Duration::from_millis(100));
    let err = lua_error(
        rt.block_on(rt.exec(
            "local co = coroutine.wrap(function() while true do end end) co()",
            "=test",
        ))
        .unwrap_err(),
    );
    assert!(was_timed_out(&err), "{err}");
}

#[test]
fn nested_coroutines_do_not_escape_the_limit_either() {
    let rt = timed(Duration::from_millis(100));
    let err = lua_error(
        rt.block_on(rt.exec(
            r#"
            local inner = function() while true do end end
            local outer = coroutine.wrap(function()
                coroutine.wrap(function()
                    coroutine.wrap(inner)()
                end)()
            end)
            outer()
            "#,
            "=test",
        ))
        .unwrap_err(),
    );
    assert!(was_timed_out(&err), "{err}");
}

#[test]
fn pcall_cannot_swallow_a_time_limit() {
    // The limit latches: once tripped, every later check fails too, so catching the error and
    // carrying on gets the script nowhere.
    let rt = timed(Duration::from_millis(100));
    let started = Instant::now();
    let err = lua_error(
        rt.block_on(rt.exec(
            "while true do pcall(function() while true do end end) end",
            "=test",
        ))
        .unwrap_err(),
    );
    assert!(was_timed_out(&err), "{err}");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "took {:?}",
        started.elapsed()
    );
}

#[test]
fn catching_the_error_once_does_not_buy_the_script_more_time() {
    let rt = timed(Duration::from_millis(100));
    let err = lua_error(
        rt.block_on(rt.exec(
            r#"
            -- The first spin is protected, so the error is caught here.
            pcall(function() while true do end end)
            -- But the latch is set, so carrying on gets nowhere.
            while true do end
            "#,
            "=test",
        ))
        .unwrap_err(),
    );
    assert!(was_timed_out(&err), "{err}");
}

#[test]
fn a_time_limit_stops_a_script_in_the_sandbox_s_setmetatable() {
    // The sandbox's `setmetatable` catches the real one's errors to raise them again from the
    // caller, and a check every instruction lands inside it. What stops the script is still the
    // time limit.
    let rt = Runtime::builder(Profile::Sandbox)
        .time_limit(Duration::from_millis(100))
        .check_interval(1)
        .build()
        .unwrap();
    let err = lua_error(
        rt.block_on(rt.exec("while true do setmetatable({}, {}) end", "=test"))
            .unwrap_err(),
    );
    assert!(was_timed_out(&err), "{err}");
}

#[test]
fn xpcall_cannot_swallow_a_cancel_either() {
    let cancel = CancelHandle::new();
    let rt = Runtime::builder(Profile::Sandbox)
        .cancel_handle(cancel.clone())
        .check_interval(1_000)
        .build()
        .unwrap();

    let watchdog = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        cancel.cancel();
    });

    let started = Instant::now();
    let err = lua_error(
        rt.block_on(rt.exec(
            "while true do xpcall(function() while true do end end, function(e) return e end) end",
            "=test",
        ))
        .unwrap_err(),
    );
    assert!(was_cancelled(&err), "{err}");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "took {:?}",
        started.elapsed()
    );
    watchdog.join().unwrap();
}

#[test]
fn a_cancel_handle_stops_a_script_from_another_thread() {
    let cancel = CancelHandle::new();
    let rt = Runtime::builder(Profile::Sandbox)
        .cancel_handle(cancel.clone())
        .check_interval(1_000)
        .build()
        .unwrap();

    let watchdog = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        cancel.cancel();
    });

    let err = lua_error(
        rt.block_on(rt.exec("while true do end", "=test"))
            .unwrap_err(),
    );
    assert!(was_cancelled(&err), "{err}");
    watchdog.join().unwrap();
}

#[test]
fn cancellation_holds_until_the_handle_is_reset() {
    let cancel = CancelHandle::new();
    let rt = Runtime::builder(Profile::Sandbox)
        .cancel_handle(cancel.clone())
        .check_interval(1_000)
        .build()
        .unwrap();

    cancel.cancel();
    let err = lua_error(rt.block_on(rt.exec("local x = 1", "=test")).unwrap_err());
    assert!(was_cancelled(&err), "{err}");
    // Still refusing, because the handle is still tripped.
    assert!(rt.block_on(rt.exec("local x = 1", "=test")).is_err());

    cancel.reset();
    rt.block_on(rt.exec("local x = 1", "=test")).unwrap();
}

#[test]
fn the_runtime_hands_back_its_own_cancel_handle() {
    let rt = Runtime::builder(Profile::Sandbox)
        .cancel_handle(CancelHandle::new())
        .build()
        .unwrap();
    let handle = rt.cancel_handle().expect("a handle was configured");
    assert!(!handle.is_cancelled());
    handle.cancel();
    assert!(rt.cancel_handle().unwrap().is_cancelled());

    assert!(
        Runtime::new(Profile::Sandbox)
            .unwrap()
            .cancel_handle()
            .is_none(),
        "a runtime with no handle configured should not invent one"
    );
}

#[test]
fn a_runtime_with_no_limits_runs_unhindered() {
    let rt = Runtime::new(Profile::Trusted).unwrap();
    assert_eq!(rt.time_limit(), None);
    let n: i64 = rt
        .block_on(rt.eval(
            "local n = 0 for i = 1, 1000000 do n = n + 1 end return n",
            "=test",
        ))
        .unwrap();
    assert_eq!(n, 1_000_000);
}

#[test]
fn a_time_limit_covers_code_reached_through_require() {
    struct Endless;
    impl avarice_rt::ModuleStore for Endless {
        fn fetch(
            &self,
            _: &avarice_rt::ModuleName,
        ) -> Result<Option<avarice_rt::ModuleSource>, avarice_rt::StoreError> {
            Ok(Some(avarice_rt::ModuleSource::new(
                "while true do end",
                "memory:endless",
            )))
        }
    }

    let rt = Runtime::builder(Profile::Sandbox)
        .time_limit(Duration::from_millis(100))
        .check_interval(1_000)
        .store(Endless)
        .build()
        .unwrap();
    let err = lua_error(
        rt.block_on(rt.exec("require('endless')", "=test"))
            .unwrap_err(),
    );
    assert!(was_timed_out(&err), "{err}");
}
