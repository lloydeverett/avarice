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
fn a_timeout_is_not_lost_when_a_task_is_the_one_running_lua() {
    let output = avrt_within(
        30,
        [
            "--timeout",
            "0.3",
            "-e",
            r#"require("utils").spawn_task(function() while true do end end)"#,
        ],
    );
    assert_eq!(output.status.code(), Some(1), "{}", stderr_of(&output));
    assert!(
        stderr_of(&output).contains("time limit"),
        "{}",
        stderr_of(&output)
    );
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
        interrupted_after(std::time::Duration::ZERO, script)
    }

    /// As [`interrupted`], but lets the script settle into whatever it does next first, for a test
    /// that needs the signal to land while something is already under way.
    fn interrupted_after(settle: std::time::Duration, script: &str) -> Output {
        let mut child = spawn_piped(["-e", script]);
        let _watchdog = Watchdog::arm(child.id(), 60);

        let mut ready = String::new();
        BufReader::new(child.stdout.as_mut().unwrap())
            .read_line(&mut ready)
            .expect("avrt should print when ready");
        assert_eq!(ready, "ready\n", "the script did not start");
        std::thread::sleep(settle);

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
    fn ctrl_c_is_not_lost_when_a_task_is_the_one_running_lua() {
        // The task dies of the limit hook and the stdlib swallows its error, leaving nothing
        // outstanding; the run must still end as interrupted rather than as a success. The task
        // says it is ready itself, so the signal lands while it is the one running Lua.
        let output = interrupted_after(
            std::time::Duration::from_millis(300),
            r#"require("utils").spawn_task(function() print("ready") while true do end end)"#,
        );
        assert_eq!(output.status.code(), Some(130), "{}", stderr_of(&output));
        assert!(
            stderr_of(&output).contains("interrupted"),
            "{}",
            stderr_of(&output)
        );
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

/// What `avrt` does with escape codes when its output is not a colour terminal.
///
/// Every run here has a pipe for stdout and stderr, so it is not a terminal, and the colour
/// variables are cleared first so that whatever the person running the tests has set does not
/// decide the result.
mod colour {
    use super::*;

    const RED: &str = "\x1b[31m";
    const RESET: &str = "\x1b[0m";

    fn avrt_with(colour_env: &[(&str, &str)], args: &[&str]) -> Output {
        let mut command = Command::new(AVRT);
        command.args(args).stdin(Stdio::null());
        for var in ["NO_COLOR", "CLICOLOR", "CLICOLOR_FORCE", "TERM"] {
            command.env_remove(var);
        }
        command.envs(colour_env.iter().copied());
        command.output().expect("avrt should run")
    }

    #[test]
    fn print_drops_escape_codes_when_output_is_not_a_terminal() {
        let output = avrt_with(&[], &["-e", r#"print("\27[31mred\27[0m")"#]);
        assert!(output.status.success(), "{}", stderr_of(&output));
        assert_eq!(stdout_of(&output), "red\n");
    }

    #[test]
    fn print_keeps_escape_codes_when_colour_is_forced() {
        let output = avrt_with(
            &[("CLICOLOR_FORCE", "1")],
            &["-e", r#"print("\27[31mred\27[0m")"#],
        );
        assert!(output.status.success(), "{}", stderr_of(&output));
        assert_eq!(stdout_of(&output), format!("{RED}red{RESET}\n"));
    }

    #[test]
    fn the_ansi_module_is_filtered_like_any_other_text() {
        let output = avrt_with(
            &[],
            &[
                "-e",
                r#"local ansi = require("ansi") print(ansi.bold .. ansi.fg.red .. "x" .. ansi.reset)"#,
            ],
        );
        assert!(output.status.success(), "{}", stderr_of(&output));
        assert_eq!(stdout_of(&output), "x\n");
    }

    #[test]
    fn a_printed_table_is_plain_when_output_is_not_a_terminal() {
        // `print` highlights whatever the destination is; `avrt` removes it where it cannot be
        // taken.
        let output = avrt_with(&[], &["-e", r#"print({ "x", a = true })"#]);
        assert!(output.status.success(), "{}", stderr_of(&output));
        assert_eq!(stdout_of(&output), "{\n  \"x\",\n  a = true,\n}\n");
    }

    #[test]
    fn a_printed_table_is_highlighted_when_colour_is_forced() {
        let output = avrt_with(&[("CLICOLOR_FORCE", "1")], &["-e", r#"print({ "x" })"#]);
        assert!(output.status.success(), "{}", stderr_of(&output));
        assert_eq!(stdout_of(&output), "{\n  \x1b[32m\"x\"\x1b[0m,\n}\n");
    }

    #[test]
    fn io_write_is_left_raw() {
        // `io` is C stdio and does not go through the write sink, so it is the way to send bytes
        // exactly as they are.
        let output = avrt_with(&[], &["-e", r#"io.write("\27[31mred\27[0m\n")"#]);
        assert!(output.status.success(), "{}", stderr_of(&output));
        assert_eq!(stdout_of(&output), format!("{RED}red{RESET}\n"));
    }

    #[test]
    fn print_and_io_write_stay_in_order_through_the_filter() {
        let output = avrt_with(
            &[],
            &[
                "-e",
                r#"io.write("a") print("\27[1mb") io.write("c") print("d")"#,
            ],
        );
        assert!(output.status.success(), "{}", stderr_of(&output));
        assert_eq!(stdout_of(&output), "ab\ncd\n");
    }

    #[test]
    fn print_is_text_so_control_bytes_are_dropped_when_colour_is_off() {
        // ADRs 0009 and 0010: `print` is a text function. A NUL is not text, so the filter drops it;
        // `io.write` is how a program writes bytes it means to be taken literally.
        let output = avrt_with(&[], &["-e", r#"print("a\0b")"#]);
        assert!(output.status.success(), "{}", stderr_of(&output));
        assert_eq!(output.stdout, b"ab\n");

        let output = avrt_with(&[], &["-e", r#"io.write("a\0b\n")"#]);
        assert!(output.status.success(), "{}", stderr_of(&output));
        assert_eq!(output.stdout, b"a\0b\n");
    }

    #[test]
    fn print_drops_control_bytes_even_when_colour_is_forced() {
        // Forcing colour lets colour through and nothing else (ADR 0010).
        let output = avrt_with(&[("CLICOLOR_FORCE", "1")], &["-e", r#"print("a\0b\7c")"#]);
        assert!(output.status.success(), "{}", stderr_of(&output));
        assert_eq!(output.stdout, b"abc\n");
    }

    #[test]
    fn only_colour_survives_when_colour_is_forced() {
        // Erase the screen, set the window title, and colour: of the three only the last is kept.
        let output = avrt_with(
            &[("CLICOLOR_FORCE", "1")],
            &["-e", r#"print("a\27[2Jb\27]0;title\7c\27[31md")"#],
        );
        assert!(output.status.success(), "{}", stderr_of(&output));
        assert_eq!(stdout_of(&output), format!("abc{RED}d\n"));
    }

    #[test]
    fn other_escape_sequences_are_dropped_when_colour_is_off() {
        let output = avrt_with(&[], &["-e", r#"print("a\27[2Jb\27]0;title\7c\27[31md")"#]);
        assert!(output.status.success(), "{}", stderr_of(&output));
        assert_eq!(stdout_of(&output), "abcd\n");
    }

    #[test]
    fn the_single_character_form_of_csi_is_dropped() {
        // U+009B is CSI as one character, and a terminal in UTF-8 mode may honour it. Nothing that
        // strips only the 7-bit form keeps a colour off the terminal under `NO_COLOR`.
        for env in [&[][..], &[("NO_COLOR", "1")], &[("CLICOLOR_FORCE", "1")]] {
            let output = avrt_with(env, &["-e", r#"print("a\u{9b}31mb")"#]);
            assert!(output.status.success(), "{}", stderr_of(&output));
            assert_eq!(output.stdout, b"a31mb\n", "{env:?}");
        }
    }

    #[test]
    fn a_string_cut_in_the_middle_of_a_character_keeps_what_follows() {
        // `sub` cuts by bytes, so a program can hand `print` half a character. The newline that
        // follows it is not lost with it.
        for env in [&[][..], &[("CLICOLOR_FORCE", "1")]] {
            let output = avrt_with(env, &["-e", r#"print(("é"):sub(1, 1)) print("next")"#]);
            assert!(output.status.success(), "{}", stderr_of(&output));
            assert_eq!(stdout_of(&output), "\u{fffd}\nnext\n", "{env:?}");
        }
    }

    #[test]
    fn an_unfinished_escape_does_not_swallow_the_next_print() {
        let output = avrt_with(
            &[("CLICOLOR_FORCE", "1")],
            &["-e", r#"print("a\27[") print("\27[31mb")"#],
        );
        assert!(output.status.success(), "{}", stderr_of(&output));
        assert_eq!(stdout_of(&output), format!("a\n{RED}b\n"));
    }

    #[test]
    fn an_error_report_drops_escape_codes_when_stderr_is_not_a_terminal() {
        let output = avrt_with(&[], &["-e", r#"error("\27[31mboom\27[0m", 0)"#]);
        assert_eq!(output.status.code(), Some(1));
        let stderr = stderr_of(&output);
        assert!(stderr.contains("avrt: boom"), "{stderr}");
        assert!(!stderr.contains('\x1b'), "{stderr:?}");
    }

    #[test]
    fn an_error_report_keeps_escape_codes_when_colour_is_forced() {
        let output = avrt_with(
            &[("CLICOLOR_FORCE", "1")],
            &["-e", r#"error("\27[31mboom\27[0m", 0)"#],
        );
        assert_eq!(output.status.code(), Some(1));
        assert!(stderr_of(&output).contains(&format!("{RED}boom{RESET}")));
    }

    #[test]
    fn no_color_wins_over_forcing_colour_on() {
        let output = avrt_with(
            &[("NO_COLOR", "1"), ("CLICOLOR_FORCE", "1")],
            &["-e", r#"print("\27[31mred\27[0m")"#],
        );
        assert!(output.status.success(), "{}", stderr_of(&output));
        assert_eq!(stdout_of(&output), "red\n");
    }

    #[test]
    fn io_stderr_is_left_raw_too() {
        let output = avrt_with(&[], &["-e", r#"io.stderr:write("\27[31mred\27[0m\n")"#]);
        assert!(output.status.success(), "{}", stderr_of(&output));
        assert_eq!(stderr_of(&output), format!("{RED}red{RESET}\n"));
    }
}

#[cfg(all(unix, feature = "stdlib-utils"))]
#[test]
fn env_get_gives_a_value_that_is_not_utf8_exactly() {
    // Set on `avrt`'s own environment, since changing the test process's is unsound while other
    // tests run on other threads.
    use std::os::unix::ffi::OsStrExt;
    let output = Command::new(AVRT)
        .args([
            "-e",
            "print(require('utils').env.get('AVARICE_RT_TEST_BYTES') == 'caf\\233')",
        ])
        .env(
            "AVARICE_RT_TEST_BYTES",
            std::ffi::OsStr::from_bytes(b"caf\xe9"),
        )
        .stdin(Stdio::null())
        .output()
        .expect("avrt should run");
    assert!(output.status.success(), "{}", stderr_of(&output));
    assert_eq!(stdout_of(&output), "true\n");
}
