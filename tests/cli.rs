//! End-to-end tests for the `avrt` command.
#![cfg(feature = "cli")]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

const AVRT: &str = env!("CARGO_BIN_EXE_avrt");

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "avrt-cli-test-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        TempDir(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn write(&self, relative: &str, contents: &str) -> PathBuf {
        let path = self.0.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, contents).unwrap();
        path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

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
