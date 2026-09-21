//! End-to-end tests for the `avrt` command.
#![cfg(feature = "cli")]

mod common;

use std::io::Write;
use std::process::{Command, Output, Stdio};

use common::TempDir;

const AVRT: &str = env!("CARGO_BIN_EXE_avrt");

fn avrt<I, S>(args: I) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    Command::new(AVRT)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("avrt should run")
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn prints_its_version() {
    let output = avrt(["-v"]);
    assert!(output.status.success());
    let stdout = stdout_of(&output);
    assert!(stdout.contains("avrt "), "{stdout}");
    assert!(stdout.contains("Lua 5.4"), "{stdout}");
}

#[test]
fn runs_a_statement_given_on_the_command_line() {
    let output = avrt(["-e", "print(6 * 7)"]);
    assert!(output.status.success(), "{}", stderr_of(&output));
    assert_eq!(stdout_of(&output), "42\n");
}

#[test]
fn print_interleaves_with_io_write_on_a_pipe() {
    // `io.write` is buffered by C stdio and `print` is not, so without care the pipe would see
    // `b` before `a`. Stock Lua gets this right because both go through the same buffer.
    let output = avrt(["-e", r#"io.write("a") print("b") io.write("c") print("d")"#]);
    assert!(output.status.success(), "{}", stderr_of(&output));
    assert_eq!(stdout_of(&output), "ab\ncd\n");
}

#[test]
fn statements_run_in_order_and_before_the_script() {
    let dir = TempDir::new();
    let script = dir.write("main.lua", "print('script', x)");
    let output = avrt(["-e", "x = 1", "-e", "x = x + 1", script.to_str().unwrap()]);
    assert!(output.status.success(), "{}", stderr_of(&output));
    assert_eq!(stdout_of(&output), "script\t2\n");
}

#[test]
fn builds_the_arg_table_the_way_stock_lua_does() {
    let dir = TempDir::new();
    let script = dir.write(
        "main.lua",
        r#"
        print(arg[0])
        print(arg[1], arg[2], arg[3])
        print(arg[-1], arg[-2] ~= nil, arg[-3])
        print(...)
        "#,
    );
    let output = avrt(["--", script.to_str().unwrap(), "a", "b"]);
    assert!(output.status.success(), "{}", stderr_of(&output));
    let stdout = stdout_of(&output);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines[0], script.to_str().unwrap());
    assert_eq!(lines[1], "a\tb\tnil");
    // arg[-1] is the word before the script, arg[-2] the interpreter, and nothing precedes it.
    assert_eq!(lines[2], "--\ttrue\tnil");
    // The script's own varargs are its arguments, not its name.
    assert_eq!(lines[3], "a\tb");
}

#[test]
fn everything_after_the_script_belongs_to_the_script() {
    let dir = TempDir::new();
    let script = dir.write("main.lua", "print(arg[1], arg[2])");
    let output = avrt([script.to_str().unwrap(), "--sandbox", "-e"]);
    assert!(output.status.success(), "{}", stderr_of(&output));
    assert_eq!(stdout_of(&output), "--sandbox\t-e\n");
}

#[test]
fn resolves_require_against_the_scripts_own_directory() {
    let dir = TempDir::new();
    dir.write("lib/greet.lua", "return function(n) return 'hi ' .. n end");
    let script = dir.write("main.lua", "print(require('lib.greet')('there'))");

    // Run from somewhere else entirely, to show it is the script's directory that matters.
    let output = Command::new(AVRT)
        .arg(script.to_str().unwrap())
        .current_dir(std::env::temp_dir())
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr_of(&output));
    assert_eq!(stdout_of(&output), "hi there\n");
}

#[test]
fn an_explicit_path_overrides_the_script_directory() {
    let dir = TempDir::new();
    let modules = TempDir::new();
    modules.write("only_here.lua", "return 'found'");
    let script = dir.write("main.lua", "print(require('only_here'))");

    let output = avrt([
        "--path",
        modules.path().to_str().unwrap(),
        script.to_str().unwrap(),
    ]);
    assert!(output.status.success(), "{}", stderr_of(&output));
    assert_eq!(stdout_of(&output), "found\n");
}

#[test]
fn the_sandbox_flag_withholds_the_dangerous_libraries() {
    let output = avrt(["--sandbox", "-e", "print(io, os, package)"]);
    assert!(output.status.success(), "{}", stderr_of(&output));
    assert_eq!(stdout_of(&output), "nil\tnil\tnil\n");

    let output = avrt(["-e", "print(io ~= nil, os ~= nil)"]);
    assert_eq!(stdout_of(&output), "true\ttrue\n");
}

#[test]
fn a_lua_error_exits_one_with_a_traceback() {
    let output = avrt(["-e", "error('boom')"]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = stderr_of(&output);
    assert!(stderr.starts_with("avrt: "), "{stderr}");
    assert!(stderr.contains("boom"), "{stderr}");
    assert!(stderr.contains("stack traceback"), "{stderr}");
}

#[test]
fn a_missing_script_exits_one() {
    let output = avrt(["definitely-not-here.lua"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr_of(&output).contains("cannot open"),
        "{}",
        stderr_of(&output)
    );
}

#[test]
fn an_unknown_option_exits_two() {
    let output = avrt(["--not-an-option"]);
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn a_bad_timeout_exits_two() {
    let output = avrt(["--timeout", "0", "-e", "print(1)"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(
        stderr_of(&output).contains("positive"),
        "{}",
        stderr_of(&output)
    );
}

#[test]
fn a_timeout_stops_a_spinning_script() {
    let started = std::time::Instant::now();
    let output = avrt(["--timeout", "0.2", "-e", "while true do end"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr_of(&output).contains("time limit"),
        "{}",
        stderr_of(&output)
    );
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
}

#[test]
fn reads_a_program_from_standard_input() {
    for args in [vec![], vec!["-"]] {
        let mut child = Command::new(AVRT)
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(b"print('piped in')")
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success(), "{args:?}: {}", stderr_of(&output));
        assert_eq!(stdout_of(&output), "piped in\n", "{args:?}");
    }
}

#[test]
fn a_shebang_line_does_not_upset_line_numbers() {
    let dir = TempDir::new();
    let script = dir.write("main.lua", "#!/usr/bin/env avrt\nerror('on line two')\n");
    let output = avrt([script.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr_of(&output).contains(":2:"), "{}", stderr_of(&output));
}

/// Runs `avrt`, killing it if it has not finished in `seconds`, so that a test of something that
/// should end does not hang the suite when it does not.
fn avrt_within<I, S>(seconds: u64, args: I) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let child = spawn_piped(args);
    let _watchdog = Watchdog::arm(child.id(), seconds);
    child.wait_with_output().expect("avrt should finish")
}

fn spawn_piped<I, S>(args: I) -> std::process::Child
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    Command::new(AVRT)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("avrt should run")
}

/// Kills a process after a while unless dropped first.
struct Watchdog(std::sync::mpsc::Sender<()>);

impl Watchdog {
    fn arm(pid: u32, seconds: u64) -> Self {
        let (disarm, armed) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            if armed
                .recv_timeout(std::time::Duration::from_secs(seconds))
                .is_err()
            {
                let _ = Command::new("kill")
                    .args(["-KILL", &pid.to_string()])
                    .status();
            }
        });
        Watchdog(disarm)
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}

#[test]
fn a_script_waits_for_the_tasks_it_left_running() {
    let dir = TempDir::new();
    let script = dir.write(
        "main.lua",
        r#"
        require("utils").spawn_timeout(function() print("late") end, 150)
        print("first")
        "#,
    );
    let output = avrt_within(30, [&script]);
    assert!(output.status.success(), "{}", stderr_of(&output));
    assert_eq!(stdout_of(&output), "first\nlate\n");
}

#[test]
fn a_statement_waits_for_its_tasks_too() {
    let output = avrt_within(
        30,
        [
            "-e",
            r#"require("utils").spawn_timeout(function() print("late") end, 150)"#,
        ],
    );
    assert!(output.status.success(), "{}", stderr_of(&output));
    assert_eq!(stdout_of(&output), "late\n");
}

#[test]
fn tasks_that_spawn_tasks_are_waited_for() {
    let output = avrt_within(
        30,
        [
            "-e",
            r#"
            local utils = require("utils")
            utils.spawn_timeout(function()
              print("outer")
              utils.spawn_timeout(function() print("inner") end, 50)
            end, 50)
            "#,
        ],
    );
    assert!(output.status.success(), "{}", stderr_of(&output));
    assert_eq!(stdout_of(&output), "outer\ninner\n");
}

#[test]
fn a_task_can_be_aborted_so_that_the_script_ends() {
    let output = avrt_within(
        30,
        [
            "-e",
            r#"
            local task = require("utils").spawn_interval(function() print("tick") end, 20)
            task:abort()
            print("done")
            "#,
        ],
    );
    assert!(output.status.success(), "{}", stderr_of(&output));
    assert_eq!(stdout_of(&output), "done\n");
}

#[test]
fn a_stdlib_module_works_from_a_script() {
    let output = avrt_within(30, ["-e", r#"print(#require("utils").uuid())"#]);
    assert!(output.status.success(), "{}", stderr_of(&output));
    assert_eq!(stdout_of(&output), "36\n");
}

#[test]
fn the_sandbox_has_no_stdlib_modules_to_spawn_tasks_with() {
    let output = avrt_within(30, ["--sandbox", "-e", r#"require("utils")"#]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr_of(&output).contains("utils"),
        "{}",
        stderr_of(&output)
    );
}

#[test]
fn a_failing_script_gives_up_its_tasks_and_says_so() {
    // An interval never ends by itself, so if the script waited for it this would hang.
    let output = avrt_within(
        30,
        [
            "-e",
            r#"require("utils").spawn_interval(function() end, 10) error("boom")"#,
        ],
    );
    assert_eq!(output.status.code(), Some(1));
    let stderr = stderr_of(&output);
    assert!(stderr.contains("boom"), "{stderr}");
    assert!(stderr.contains("aborting 1 running task\n"), "{stderr}");
}

#[test]
fn a_timeout_ends_the_wait_for_tasks() {
    let started = std::time::Instant::now();
    let output = avrt_within(
        30,
        [
            "--timeout",
            "0.3",
            "-e",
            r#"require("utils").spawn_interval(function() end, 10)"#,
        ],
    );
    assert_eq!(output.status.code(), Some(1));
    let stderr = stderr_of(&output);
    assert!(stderr.contains("time limit"), "{stderr}");
    assert!(stderr.contains("aborting 1 running task"), "{stderr}");
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
}

/// Ctrl-C at a terminal is a SIGINT to `avrt`. These start a script that says it is ready once
/// the handler stands, so that the signal never races it, then send the signal and see what is
/// left. They are unix-only because `kill` is.
#[cfg(unix)]
mod interrupt {
    use super::*;
    use std::io::{BufRead, BufReader};

    /// Starts `script`, waits for it to print a line, sends SIGINT, and collects what happens.
    fn interrupted(script: &str) -> Output {
        let mut child = spawn_piped(["-e", script]);
        let _watchdog = Watchdog::arm(child.id(), 60);

        let mut ready = String::new();
        BufReader::new(child.stdout.as_mut().unwrap())
            .read_line(&mut ready)
            .expect("avrt should print when ready");
        assert_eq!(ready, "ready\n", "the script did not start");

        let status = Command::new("kill")
            .args(["-INT", &child.id().to_string()])
            .status()
            .unwrap();
        assert!(status.success());
        child.wait_with_output().expect("avrt should finish")
    }

    #[test]
    fn ctrl_c_stops_a_spinning_script() {
        let output = interrupted(r#"print("ready") while true do end"#);
        assert_eq!(output.status.code(), Some(130), "{}", stderr_of(&output));
        assert!(
            stderr_of(&output).contains("interrupted"),
            "{}",
            stderr_of(&output)
        );
    }

    #[test]
    fn ctrl_c_stops_a_script_that_is_awaiting() {
        // Nothing is running Lua here, so the hook cannot notice; the wait has to be woken.
        let output = interrupted(
            r#"print("ready") require("utils").spawn_timeout(function() end, 600000):await()"#,
        );
        assert_eq!(output.status.code(), Some(130), "{}", stderr_of(&output));
        let stderr = stderr_of(&output);
        assert!(stderr.contains("interrupted"), "{stderr}");
        assert!(stderr.contains("aborting 1 running task\n"), "{stderr}");
    }

    #[test]
    fn ctrl_c_stops_the_wait_for_tasks_and_counts_them() {
        let output = interrupted(
            r#"
            local utils = require("utils")
            utils.spawn_interval(function() end, 10)
            utils.spawn_interval(function() end, 10)
            print("ready")
            "#,
        );
        assert_eq!(output.status.code(), Some(130), "{}", stderr_of(&output));
        let stderr = stderr_of(&output);
        assert!(stderr.contains("interrupted"), "{stderr}");
        assert!(stderr.contains("aborting 2 running tasks\n"), "{stderr}");
    }
}
