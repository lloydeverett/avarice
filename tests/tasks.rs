//! What becomes of the tasks Lua code spawns: counting them, waiting for them, aborting them, and
//! stopping a runtime that is waiting on something.
//!
//! The tasks here are the stdlib's own (`utils.spawn_*`), whose handles live inside Lua; the
//! runtime has no list of them, so everything below is about what it can tell from the executor.

#![cfg(feature = "stdlib-utils")]

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use avarice_rt::{was_cancelled, was_timed_out, CancelHandle, Error, Profile, Runtime, StdModules};

fn trusted() -> Runtime {
    Runtime::new(Profile::Trusted).unwrap()
}

fn lua_error(err: Error) -> avarice_rt::mlua::Error {
    match err {
        Error::Lua(err) => err,
        other => panic!("expected a Lua error, got {other:?}"),
    }
}

fn count(rt: &Runtime) -> i64 {
    rt.block_on(rt.eval("return ran or 0", "=count")).unwrap()
}

#[test]
fn a_runtime_that_spawned_nothing_has_no_tasks() {
    let rt = trusted();
    assert_eq!(rt.outstanding_tasks(), 0);
    rt.block_on(rt.wait_for_tasks()).unwrap();
}

#[test]
fn a_task_is_outstanding_until_it_has_run() {
    let rt = trusted();
    rt.block_on(rt.exec(
        r#"require("utils").spawn_timeout(function() ran = (ran or 0) + 1 end, 30)"#,
        "=spawn",
    ))
    .unwrap();
    assert_eq!(rt.outstanding_tasks(), 1);
    assert_eq!(count(&rt), 0, "the timer has not fired yet");

    rt.block_on(rt.wait_for_tasks()).unwrap();
    assert_eq!(rt.outstanding_tasks(), 0);
    assert_eq!(count(&rt), 1);
}

#[test]
fn waiting_covers_a_task_that_spawns_another() {
    // The rule is "no task left", not "the tasks that existed when you asked".
    let rt = trusted();
    rt.block_on(rt.exec(
        r#"
        local utils = require("utils")
        utils.spawn_timeout(function()
          ran = (ran or 0) + 1
          utils.spawn_timeout(function() ran = ran + 1 end, 30)
        end, 30)
        "#,
        "=spawn",
    ))
    .unwrap();
    rt.block_on(rt.wait_for_tasks()).unwrap();
    assert_eq!(count(&rt), 2);
}

#[test]
fn waiting_is_bounded_by_the_time_limit() {
    // An interval never finishes, so this is the only thing that ends the wait.
    let rt = Runtime::builder(Profile::Sandbox)
        .with_std_modules(StdModules::UTILS)
        .time_limit(Duration::from_millis(150))
        .build()
        .unwrap();
    rt.block_on(rt.exec(
        r#"require("utils").spawn_interval(function() end, 10)"#,
        "=spawn",
    ))
    .unwrap();

    let started = Instant::now();
    let err = lua_error(rt.block_on(rt.wait_for_tasks()).unwrap_err());
    assert!(was_timed_out(&err), "{err}");
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(rt.outstanding_tasks(), 1, "waiting does not abort");
}

#[test]
fn waiting_stops_when_cancelled() {
    let cancel = CancelHandle::new();
    let rt = Runtime::builder(Profile::Trusted)
        .cancel_handle(cancel.clone())
        .build()
        .unwrap();
    rt.block_on(rt.exec(
        r#"require("utils").spawn_interval(function() end, 10)"#,
        "=spawn",
    ))
    .unwrap();

    let canceller = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        cancel.cancel();
    });
    let started = Instant::now();
    let err = lua_error(rt.block_on(rt.wait_for_tasks()).unwrap_err());
    canceller.join().unwrap();

    assert!(was_cancelled(&err), "{err}");
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn a_task_that_dies_of_the_time_limit_does_not_read_as_finishing() {
    // The limit hook stops the task's Lua, and the stdlib swallows a task's error, so afterwards
    // the executor simply has one task fewer. The wait must not mistake that for a clean finish.
    let rt = Runtime::builder(Profile::Sandbox)
        .with_std_modules(StdModules::UTILS)
        .time_limit(Duration::from_millis(150))
        .check_interval(1_000)
        .build()
        .unwrap();
    rt.block_on(rt.exec(
        r#"require("utils").spawn_task(function() while true do end end)"#,
        "=spawn",
    ))
    .unwrap();

    let err = lua_error(rt.block_on(rt.wait_for_tasks()).unwrap_err());
    assert!(was_timed_out(&err), "{err}");
    assert_eq!(rt.outstanding_tasks(), 0, "the task did die");
}

#[test]
fn a_task_that_dies_of_a_cancel_does_not_read_as_finishing() {
    let cancel = CancelHandle::new();
    let rt = Runtime::builder(Profile::Trusted)
        .cancel_handle(cancel.clone())
        .check_interval(1_000)
        .build()
        .unwrap();
    rt.block_on(rt.exec(
        r#"require("utils").spawn_task(function() while true do end end)"#,
        "=spawn",
    ))
    .unwrap();

    let canceller = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        cancel.cancel();
    });
    let err = lua_error(rt.block_on(rt.wait_for_tasks()).unwrap_err());
    canceller.join().unwrap();
    assert!(was_cancelled(&err), "{err}");
}

#[test]
fn aborting_stops_the_tasks_and_counts_them() {
    let rt = trusted();
    rt.block_on(rt.exec(
        r#"
        local utils = require("utils")
        for _ = 1, 3 do
          utils.spawn_interval(function() ran = (ran or 0) + 1 end, 5)
        end
        "#,
        "=spawn",
    ))
    .unwrap();
    // Let them get going, so that they are running rather than merely spawned.
    rt.block_on(async { tokio::time::sleep(Duration::from_millis(50)).await });
    assert!(count(&rt) > 0);

    assert_eq!(rt.abort_tasks().unwrap(), 3);
    assert_eq!(rt.outstanding_tasks(), 0);

    let before = count(&rt);
    rt.block_on(async { tokio::time::sleep(Duration::from_millis(50)).await });
    assert_eq!(count(&rt), before, "an aborted task ran again");
}

#[test]
fn aborting_drops_what_the_tasks_held() {
    struct Guard(Arc<AtomicBool>);
    impl Drop for Guard {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    let dropped = Arc::new(AtomicBool::new(false));
    let guard = Guard(Arc::clone(&dropped));
    let rt = trusted();
    rt.block_on(async {
        tokio::spawn(async move {
            let _guard = guard;
            std::future::pending::<()>().await
        });
        tokio::task::yield_now().await;
    });
    assert!(!dropped.load(Ordering::SeqCst));

    assert_eq!(rt.abort_tasks().unwrap(), 1);
    assert!(
        dropped.load(Ordering::SeqCst),
        "the task outlived the abort"
    );
}

#[test]
fn a_runtime_is_usable_after_aborting_its_tasks() {
    let rt = trusted();
    rt.block_on(rt.exec(
        r#"require("utils").spawn_interval(function() end, 5)"#,
        "=spawn",
    ))
    .unwrap();
    assert_eq!(rt.abort_tasks().unwrap(), 1);

    // The same Lua state, with its globals, and a working executor: new tasks spawn, run and end.
    rt.block_on(rt.exec("kept = 42", "=set")).unwrap();
    rt.block_on(rt.exec(
        r#"require("utils").spawn_timeout(function() ran = 1 end, 10)"#,
        "=spawn",
    ))
    .unwrap();
    rt.block_on(rt.wait_for_tasks()).unwrap();
    assert_eq!(count(&rt), 1);
    let kept: i64 = rt.block_on(rt.eval("return kept", "=read")).unwrap();
    assert_eq!(kept, 42);
}

#[test]
fn aborting_with_nothing_running_is_a_no_op() {
    let rt = trusted();
    assert_eq!(rt.abort_tasks().unwrap(), 0);
    assert_eq!(rt.block_on(async { 6 * 7 }), 42);
}

#[test]
fn aborting_from_inside_the_executor_panics_rather_than_deadlocking() {
    let rt = trusted();
    let panic = catch_unwind(AssertUnwindSafe(|| rt.block_on(async { rt.abort_tasks() })))
        .expect_err("aborting while the executor is running should panic");

    let message = panic
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_default();
    assert!(message.contains("abort_tasks"), "{message}");
    // The panic did not leave the runtime unusable.
    assert_eq!(rt.block_on(async { 1 }), 1);
}

#[test]
fn cancelling_wakes_a_chunk_that_is_waiting_on_something() {
    // Nothing is executing Lua here, so the hook cannot notice; the executor has to be woken.
    let cancel = CancelHandle::new();
    let rt = Runtime::builder(Profile::Trusted)
        .cancel_handle(cancel.clone())
        .build()
        .unwrap();

    let canceller = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        cancel.cancel();
    });
    let started = Instant::now();
    let err = lua_error(
        rt.block_on(rt.exec(
            r#"require("utils").spawn_timeout(function() end, 60000):await()"#,
            "=wait",
        ))
        .unwrap_err(),
    );
    canceller.join().unwrap();

    assert!(was_cancelled(&err), "{err}");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "took {:?}",
        started.elapsed()
    );
}

#[test]
fn a_cancelled_evaluation_does_not_poison_the_session() {
    let cancel = CancelHandle::new();
    let rt = Runtime::builder(Profile::Trusted)
        .cancel_handle(cancel.clone())
        .check_interval(1_000)
        .build()
        .unwrap();
    rt.block_on(rt.exec("kept = 42", "=set")).unwrap();

    // Once by the hook, while Lua is running...
    let canceller = {
        let cancel = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            cancel.cancel();
        })
    };
    let err = lua_error(
        rt.block_on(rt.exec("while true do end", "=spin"))
            .unwrap_err(),
    );
    canceller.join().unwrap();
    assert!(was_cancelled(&err), "{err}");

    // ...refused until the handle is reset, which is what a REPL does before each entry...
    let err = lua_error(rt.block_on(rt.exec("x = 1", "=refused")).unwrap_err());
    assert!(was_cancelled(&err), "{err}");

    // ...and then the runtime carries on with its state intact.
    cancel.reset();
    let kept: i64 = rt.block_on(rt.eval("return kept", "=read")).unwrap();
    assert_eq!(kept, 42);

    // Once more, this time by waking an await.
    let canceller = {
        let cancel = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            cancel.cancel();
        })
    };
    let err = lua_error(
        rt.block_on(rt.exec(
            r#"require("utils").spawn_timeout(function() end, 60000):await()"#,
            "=wait",
        ))
        .unwrap_err(),
    );
    canceller.join().unwrap();
    assert!(was_cancelled(&err), "{err}");
    cancel.reset();
    let kept: i64 = rt.block_on(rt.eval("return kept", "=read")).unwrap();
    assert_eq!(kept, 42);
}
