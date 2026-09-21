//! Execution is asynchronous, and the runtime owns the executor that drives it.
//!
//! No `#[tokio::test]` here or anywhere: the runtime brings its own executor, and these tests get
//! the same deal an embedder does. The one test that needs a second tokio runtime builds it by
//! hand, because building it is the point.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use avarice_rt::{was_timed_out, Error, Profile, Runtime, StdModules};

fn trusted() -> Runtime {
    Runtime::new(Profile::Trusted).unwrap()
}

#[test]
fn a_chunk_can_await_an_async_stdlib_function() {
    // `await` is an async method on a task handle, so this only works if the chunk is being run
    // as a coroutine that something is driving.
    let rt = trusted();
    let done: bool = rt
        .block_on(rt.eval(
            r#"
            local utils = require("utils")
            local done = false
            utils.spawn_task(function() done = true end):await()
            return done
            "#,
            "=test",
        ))
        .unwrap();
    assert!(done);
}

#[test]
fn the_executor_has_a_timer() {
    // A task that sleeps needs tokio's time driver, which `enable_all` turns on.
    let rt = trusted();
    let started = Instant::now();
    let done: bool = rt
        .block_on(rt.eval(
            r#"
            local utils = require("utils")
            local done = false
            utils.spawn_timeout(function() done = true end, 50):await()
            return done
            "#,
            "=test",
        ))
        .unwrap();
    assert!(done);
    assert!(started.elapsed() >= Duration::from_millis(50));
}

#[test]
fn await_works_under_pcall() {
    // Lua 5.4's `pcall` is yieldable, so an await inside one must not fail with "attempt to
    // yield across a C-call boundary".
    let rt = trusted();
    let done: bool = rt
        .block_on(rt.eval(
            r#"
            local utils = require("utils")
            local done = false
            local ok = pcall(function()
              utils.spawn_task(function() done = true end):await()
            end)
            return ok and done
            "#,
            "=test",
        ))
        .unwrap();
    assert!(done);
}

#[test]
fn block_on_drives_any_future_to_completion() {
    let rt = trusted();
    assert_eq!(rt.block_on(async { 6 * 7 }), 42);
    let slept = rt.block_on(async {
        tokio::time::sleep(Duration::from_millis(5)).await;
        "woke"
    });
    assert_eq!(slept, "woke");
}

#[test]
fn a_task_spawned_during_the_call_runs_during_the_call() {
    let rt = trusted();
    let hit: bool = rt
        .block_on(async {
            rt.exec(
                r#"
                hit = false
                require("utils").spawn_timeout(function() hit = true end, 10)
                "#,
                "=spawn",
            )
            .await?;
            // Nothing awaits the task; it runs because the executor is being driven.
            tokio::time::sleep(Duration::from_millis(100)).await;
            rt.eval("return hit", "=read").await
        })
        .unwrap();
    assert!(hit);
}

#[test]
fn a_task_is_not_driven_between_calls() {
    // The executor is only running while `block_on` is. This is what a REPL that drains tasks
    // between prompts has to work around, and what ticket 11 builds on.
    let rt = trusted();
    rt.block_on(rt.exec(
        r#"
        hit = false
        require("utils").spawn_timeout(function() hit = true end, 10)
        "#,
        "=spawn",
    ))
    .unwrap();
    std::thread::sleep(Duration::from_millis(100));
    let hit: bool = rt.block_on(rt.eval("return hit", "=read")).unwrap();
    assert!(!hit, "a task ran while nothing was driving the executor");
}

#[test]
fn block_on_inside_another_tokio_runtime_panics_with_tokios_own_message() {
    let rt = trusted();
    let outer = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let panic = catch_unwind(AssertUnwindSafe(|| {
        outer.block_on(async { rt.block_on(async {}) })
    }))
    .expect_err("block_on inside a tokio runtime should panic");

    let message = panic
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_default();
    assert!(
        message.contains("Cannot start a runtime from within a runtime"),
        "{message}"
    );
}

#[test]
fn dropping_a_runtime_does_not_wait_for_its_tasks() {
    let rt = trusted();
    rt.block_on(rt.exec(
        r#"require("utils").spawn_interval(function() end, 10)"#,
        "=spawn",
    ))
    .unwrap();

    let started = Instant::now();
    drop(rt);
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "dropping waited {:?}",
        started.elapsed()
    );
}

#[test]
fn dropping_a_runtime_stops_its_tasks() {
    // The interval's callback calls into Rust, so the count says whether it is still being run.
    let ticks = Arc::new(AtomicUsize::new(0));
    let rt = trusted();
    let counter = Arc::clone(&ticks);
    rt.lua()
        .globals()
        .set(
            "tick",
            rt.lua()
                .create_function(move |_, ()| {
                    counter.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })
                .unwrap(),
        )
        .unwrap();
    rt.block_on(async {
        rt.exec(
            r#"require("utils").spawn_interval(function() tick() end, 5)"#,
            "=spawn",
        )
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
    });
    assert!(ticks.load(Ordering::SeqCst) > 0, "the interval never ran");

    drop(rt);
    let at_drop = ticks.load(Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(ticks.load(Ordering::SeqCst), at_drop);
}

#[test]
fn a_time_limit_is_still_armed_after_an_await() {
    // The clock runs while the chunk is awaiting, so a chunk that waits out its budget is stopped
    // as soon as it executes anything again. ADR 0004 accepts this.
    let rt = Runtime::builder(Profile::Sandbox)
        .with_std_modules(StdModules::UTILS)
        .time_limit(Duration::from_millis(50))
        .check_interval(1_000)
        .build()
        .unwrap();
    let err = rt
        .block_on(rt.exec(
            r#"
            require("utils").spawn_timeout(function() end, 200):await()
            for i = 1, 100 do tostring(i) end
            "#,
            "=test",
        ))
        .unwrap_err();
    let Error::Lua(err) = err else {
        panic!("expected a Lua error, got {err:?}")
    };
    assert!(was_timed_out(&err), "{err}");
}
