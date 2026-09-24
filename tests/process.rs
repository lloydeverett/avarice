//! The `process` module (ADR 0016), driven the way a script does: Lua through a trusted runtime.
//!
//! The Child every test runs is `avrt` itself, given a line of Lua, so that it behaves the same on
//! every platform and no test leans on a shell or on `echo`, `cat` or `sleep`. That is why these
//! need the `cli` feature, for the binary, and `utils`, which the Lua given to it uses to wait.

#![cfg(all(
    feature = "cli",
    feature = "stdlib-process",
    feature = "stdlib-utils"
))]

mod common;

use std::path::Path;
use std::time::{Duration, Instant};

use avarice_rt::{CancelHandle, Profile, Runtime};
use common::{Heartbeat, TempDir};

const AVRT: &str = env!("CARGO_BIN_EXE_avrt");

/// A trusted runtime with `process` and `utils` required, `AVRT` set to the binary's path, and
/// `avrt(source, fields)`, a Command that runs `source` in it with `fields` added.
fn runtime() -> Runtime {
    prepared(Runtime::new(Profile::Trusted).unwrap())
}

/// `rt`, prepared as [`runtime`] prepares its own.
fn prepared(rt: Runtime) -> Runtime {
    rt.lua().globals().set("AVRT", AVRT).unwrap();
    rt.block_on(rt.exec(
        r#"
        process = require("process")
        utils = require("utils")
        function avrt(source, fields)
          local command = { AVRT, "-e", source }
          for key, value in pairs(fields or {}) do command[key] = value end
          return command
        end
        "#,
        "=prelude",
    ))
    .unwrap();
    rt
}

fn eval<R: avarice_rt::mlua::FromLuaMulti>(rt: &Runtime, source: &str) -> R {
    rt.block_on(rt.eval(source, "=test"))
        .unwrap_or_else(|e| panic!("{e}"))
}

/// The message of the error `source` raises, which must be one.
fn error_of(rt: &Runtime, source: &str) -> String {
    match rt.block_on(rt.exec(source, "=test")) {
        Ok(()) => panic!("expected an error from {source}"),
        Err(e) => e.to_string(),
    }
}

/// A Lua long string holding `text` exactly.
fn lua_string(text: &str) -> String {
    assert!(!text.contains("]==]"));
    format!("[==[{text}]==]")
}

fn path_string(path: &Path) -> String {
    lua_string(path.to_str().unwrap())
}

/// A Command, as Lua source, that beats `heartbeat` until it is killed.
fn beating(heartbeat: &Heartbeat) -> String {
    format!(
        "avrt({}, {{ env = {{ BEAT = {} }} }})",
        lua_string(Heartbeat::LUA),
        path_string(heartbeat.path())
    )
}

/// A copy of `avrt` named `avrt-copy` in a directory of its own, for finding by name.
fn avrt_copy() -> TempDir {
    let dir = TempDir::new();
    let name = format!("avrt-copy{}", std::env::consts::EXE_SUFFIX);
    std::fs::copy(AVRT, dir.path().join(name)).unwrap();
    dir
}

// A Command.

#[test]
fn a_bare_string_command_is_refused_with_the_table_form_in_the_message() {
    let rt = runtime();
    for function in ["run", "spawn"] {
        let message = error_of(&rt, &format!(r#"process.{function}("ls -la")"#));
        assert!(message.contains(r#"{ "ls", "-la" }"#), "{message}");
        assert!(message.contains(&format!("process.{function}")), "{message}");
    }
}

#[test]
fn a_command_with_no_program_is_refused() {
    let rt = runtime();
    let message = error_of(&rt, r#"process.run({ cwd = "." })"#);
    assert!(message.contains("program"), "{message}");
}

#[test]
fn a_field_a_command_does_not_have_is_refused() {
    // A misspelt option is input the module cannot use, so it raises rather than being ignored
    // (ADR 0017).
    let rt = runtime();
    let message = error_of(&rt, r#"process.run(avrt("", { timout = 5 }))"#);
    assert!(message.contains("timout"), "{message}");
}

#[test]
fn run_only_fields_are_refused_by_spawn() {
    let rt = runtime();
    for field in ["check = true", "timeout = 5"] {
        let message = error_of(&rt, &format!(r#"process.spawn(avrt("", {{ {field} }}))"#));
        assert!(message.contains("process.run"), "{message}");
    }
}

#[test]
fn a_bad_stream_setting_is_refused() {
    let rt = runtime();
    let message = error_of(&rt, r#"process.run(avrt("", { stdout = "pipes" }))"#);
    assert!(message.contains("stdout"), "{message}");
    let message = error_of(&rt, r#"process.run(avrt("", { stdio = "stdout" }))"#);
    assert!(message.contains("stdio"), "{message}");
}

#[test]
fn an_argument_that_is_not_a_string_is_refused() {
    let rt = runtime();
    let message = error_of(&rt, r#"process.run({ AVRT, "-e", true })"#);
    assert!(message.contains("entry 3"), "{message}");
}

#[test]
fn arguments_arrive_one_by_one_and_unchanged() {
    let dir = TempDir::new();
    let script = dir.write(
        "args.lua",
        r#"for i = 1, #arg do io.write(arg[i], "|") end"#,
    );
    let rt = runtime();
    let output: String = eval(
        &rt,
        &format!(
            r#"return process.run({{ AVRT, {}, "two words", [["quoted" 'too']], "$HOME", "*", "" }})
                 .stdout:bytes()"#,
            path_string(&script)
        ),
    );
    assert_eq!(output, r#"two words|"quoted" 'too'|$HOME|*||"#);
}

#[cfg(unix)]
#[test]
fn an_argument_that_is_not_utf8_arrives_as_its_bytes() {
    // `avrt` refuses such an argument itself, so the Child here is POSIX `printf`.
    let rt = runtime();
    let exact: bool = eval(
        &rt,
        r#"return process.run({ "printf", "%s", "\255x\128" }).stdout:bytes() == "\255x\128""#,
    );
    assert!(exact);
}

#[test]
fn an_argument_holding_a_nul_is_refused() {
    // No operating system can pass one: the argument would end there.
    let rt = runtime();
    let message = error_of(&rt, r#"process.run({ AVRT, "-e", "a\0b" })"#);
    assert!(message.contains("entry 3"), "{message}");
}

// Output.

#[test]
fn stdout_and_stderr_come_back_separately_with_the_exit_code() {
    let rt = runtime();
    let (ok, code, stdout, stderr): (bool, i64, String, String) = eval(
        &rt,
        r#"
        local output = process.run(avrt([[io.write("out") io.stderr:write("err")]]))
        return output.ok, output.code, output.stdout:bytes(), output.stderr:bytes()
        "#,
    );
    assert!(ok);
    assert_eq!(code, 0);
    assert_eq!(stdout, "out");
    assert_eq!(stderr, "err");
}

#[test]
fn merged_stderr_interleaves_with_stdout_in_write_order() {
    let rt = runtime();
    let (stdout, stderr_length): (String, i64) = eval(
        &rt,
        r#"
        local output = process.run(avrt([[
          io.stdout:setvbuf("no")
          io.write("a") io.stderr:write("b") io.write("c") io.stderr:write("d")
        ]], { stderr = "stdout" }))
        return output.stdout:bytes(), #output.stderr
        "#,
    );
    assert_eq!(stdout, "abcd");
    assert_eq!(stderr_length, 0);
}

#[test]
fn output_that_is_not_utf8_survives_exactly() {
    let rt = runtime();
    let (exact, length): (bool, i64) = eval(
        &rt,
        r#"
        local output = process.run(avrt([[io.write("\0\255\1\128")]]))
        return output.stdout:bytes() == "\0\255\1\128", #output.stdout
        "#,
    );
    assert!(exact);
    assert_eq!(length, 4);
}

#[test]
fn a_stream_that_is_not_piped_gives_an_empty_buffer() {
    let rt = runtime();
    let (stdout, stderr): (i64, i64) = eval(
        &rt,
        r#"
        local output = process.run(avrt([[io.write("x") io.stderr:write("y")]], { stdio = "null" }))
        return #output.stdout, #output.stderr
        "#,
    );
    assert_eq!((stdout, stderr), (0, 0));
}

#[test]
fn a_per_stream_setting_wins_over_stdio() {
    let rt = runtime();
    let (stdout, stderr): (String, i64) = eval(
        &rt,
        r#"
        local output = process.run(avrt([[io.write("x") io.stderr:write("y")]],
          { stdio = "null", stdout = "pipe" }))
        return output.stdout:bytes(), #output.stderr
        "#,
    );
    assert_eq!((stdout.as_str(), stderr), ("x", 0));
}

#[test]
fn an_unsuccessful_exit_is_returned_not_raised() {
    let rt = runtime();
    let (ok, code, signal): (bool, i64, Option<i64>) = eval(
        &rt,
        r#"
        local output = process.run(avrt("os.exit(3)"))
        return output.ok, output.code, output.signal
        "#,
    );
    assert!(!ok);
    assert_eq!(code, 3);
    assert_eq!(signal, None);
}

#[test]
fn check_raises_an_exit_error_carrying_the_output() {
    let rt = runtime();
    let (kind, program, code, stderr, message): (String, String, i64, String, String) = eval(
        &rt,
        r#"
        local ok, err = pcall(process.run,
          avrt([[io.stderr:write("fatal: no") os.exit(128)]], { check = true }))
        assert(not ok)
        return err.kind, err.program, err.output.code, err.output.stderr:bytes(), tostring(err)
        "#,
    );
    assert_eq!(kind, "exit");
    assert_eq!(program, AVRT);
    assert_eq!(code, 128);
    assert_eq!(stderr, "fatal: no");
    assert_eq!(message, format!("process: {AVRT} exited with code 128"));
}

#[cfg(unix)]
#[test]
fn check_names_the_signal_that_ended_the_child() {
    // `sh` as a program, not as a shell for the Command: it kills itself.
    let rt = runtime();
    let (signal, message): (i64, String) = eval(
        &rt,
        r#"
        local ok, err = pcall(process.run, { "sh", "-c", "kill -KILL $$", check = true })
        return err.output.signal, tostring(err)
        "#,
    );
    assert_eq!(signal, 9);
    assert_eq!(message, "process: sh was killed by signal 9");
}

#[test]
fn run_refuses_a_stdin_nothing_could_write_to() {
    let rt = runtime();
    let message = error_of(&rt, r#"process.run(avrt("", { stdin = "pipe" }))"#);
    assert!(message.contains("stdin"), "{message}");
    let reads_to_the_end = r#"return process.run(avrt([[io.read("a")]], { stdio = "pipe" })).ok"#;
    let ok: bool = eval(&rt, reads_to_the_end);
    assert!(ok, "a stdin piped by stdio is closed, so the Child reads to its end");
}

#[test]
fn check_is_quiet_about_a_successful_exit() {
    let rt = runtime();
    let ok: bool = eval(&rt, r#"return process.run(avrt("", { check = true })).ok"#);
    assert!(ok);
}

// Starting.

#[test]
fn a_missing_program_is_a_start_error_saying_not_found() {
    let rt = runtime();
    let (kind, program, reason, message): (String, String, String, String) = eval(
        &rt,
        r#"
        local ok, err = pcall(process.run, { "avarice-rt-no-such-program", "--secret" })
        return err.kind, err.program, err.reason, tostring(err)
        "#,
    );
    assert_eq!(kind, "start");
    assert_eq!(program, "avarice-rt-no-such-program");
    assert_eq!(reason, "not_found");
    assert_eq!(
        message,
        "process: could not start 'avarice-rt-no-such-program': program not found"
    );
}

#[test]
fn a_missing_working_directory_is_a_start_error_saying_bad_cwd() {
    let rt = runtime();
    let dir = TempDir::new();
    let (reason, message): (String, String) = eval(
        &rt,
        &format!(
            r#"
            local ok, err = pcall(process.run, avrt("", {{ cwd = {} }}))
            return err.reason, err.message
            "#,
            path_string(&dir.path().join("nowhere"))
        ),
    );
    assert_eq!(reason, "bad_cwd");
    assert!(!message.is_empty());
}

#[cfg(unix)]
#[test]
fn a_file_that_is_not_executable_is_a_start_error_saying_permission_denied() {
    let rt = runtime();
    let dir = TempDir::new();
    dir.write("plain", "not a program");
    let reason: String = eval(
        &rt,
        &format!(
            r#"
            local ok, err = pcall(process.run, {{ "./plain", cwd = {} }})
            return err.reason
            "#,
            path_string(dir.path())
        ),
    );
    assert_eq!(reason, "permission_denied");
}

#[cfg(unix)]
#[test]
fn a_file_on_the_path_that_is_not_executable_is_a_start_error_saying_permission_denied() {
    let rt = runtime();
    let dir = TempDir::new();
    dir.write("plain", "not a program");
    let reason: String = eval(
        &rt,
        &format!(
            r#"return select(2, pcall(process.run, {{ "plain", env = {{ PATH = {} }} }})).reason"#,
            path_string(dir.path())
        ),
    );
    assert_eq!(reason, "permission_denied");
}

#[cfg(unix)]
#[test]
fn a_program_on_the_path_is_found_past_a_file_of_its_name_that_cannot_run() {
    let copy = avrt_copy();
    let dir = TempDir::new();
    dir.write("avrt-copy", "not a program");
    let rt = runtime();
    let output: String = eval(
        &rt,
        &format!(
            r#"return process.run({{ "avrt-copy", "-e", "io.write('found')",
                 env = {{ PATH = {} }} }}).stdout:bytes()"#,
            lua_string(&format!("{}:{}", dir.path().display(), copy.path().display()))
        ),
    );
    assert_eq!(output, "found");
}

#[cfg(unix)]
#[test]
fn a_directory_on_the_path_is_not_the_program() {
    let dir = TempDir::new();
    std::fs::create_dir(dir.path().join("tool")).unwrap();
    let rt = runtime();
    let reason: String = eval(
        &rt,
        &format!(
            r#"return select(2, pcall(process.run, {{ "tool", env = {{ PATH = {} }} }})).reason"#,
            path_string(dir.path())
        ),
    );
    assert_eq!(reason, "not_found");
}

#[test]
fn a_program_is_found_on_the_path_the_command_gives() {
    // On Windows the copy is `avrt-copy.exe`, so this is also `PATHEXT` at work.
    let copy = avrt_copy();
    let rt = runtime();
    let output: String = eval(
        &rt,
        &format!(
            r#"return process.run({{ "avrt-copy", "-e", "io.write('found')",
                 env = {{ PATH = {} }} }}).stdout:bytes()"#,
            path_string(copy.path())
        ),
    );
    assert_eq!(output, "found");

    let reason: String = eval(
        &rt,
        r#"return select(2, pcall(process.run, { "avrt-copy" })).reason"#,
    );
    assert_eq!(reason, "not_found", "the host's own PATH does not have it");
}

#[test]
fn a_relative_program_resolves_against_the_working_directory() {
    let copy = avrt_copy();
    let rt = runtime();
    let output: String = eval(
        &rt,
        &format!(
            r#"return process.run({{ "./avrt-copy{}", "-e", "io.write('here')",
                 cwd = {} }}).stdout:bytes()"#,
            std::env::consts::EXE_SUFFIX,
            path_string(copy.path())
        ),
    );
    assert_eq!(output, "here");
}

#[test]
fn cwd_is_where_the_child_runs() {
    let dir = TempDir::new();
    dir.write("marker.txt", "found it");
    let rt = runtime();
    let output: String = eval(
        &rt,
        &format!(
            r#"return process.run(avrt([[io.write(io.open("marker.txt"):read("a"))]],
                 {{ cwd = {} }})).stdout:bytes()"#,
            path_string(dir.path())
        ),
    );
    assert_eq!(output, "found it");
}

// The environment.

#[test]
fn env_sets_overrides_and_removes_variables() {
    let rt = runtime();
    let output: String = eval(
        &rt,
        r#"
        return process.run(avrt(
          [[io.write(os.getenv("AVRT_SET"), "|", os.getenv("PATH"), "|",
            tostring(os.getenv("HOME") or os.getenv("USERPROFILE")))]],
          { env = { AVRT_SET = "set", PATH = "overridden", HOME = false, USERPROFILE = false } }
        )).stdout:bytes()
        "#,
    );
    assert_eq!(output, "set|overridden|nil");
}

#[test]
fn clear_env_starts_the_child_with_only_what_env_gives() {
    let rt = runtime();
    let output: String = eval(
        &rt,
        r#"
        return process.run(avrt([[io.write(tostring(os.getenv("PATH")), "|", os.getenv("ONLY"))]],
          { clear_env = true, env = { ONLY = "this" } })).stdout:bytes()
        "#,
    );
    assert_eq!(output, "nil|this");
}

#[test]
fn an_env_value_that_is_not_a_string_or_false_is_refused() {
    let rt = runtime();
    let message = error_of(&rt, r#"process.run(avrt("", { env = { X = true } }))"#);
    assert!(message.contains("X"), "{message}");
}

// Standard input.

#[test]
fn run_gives_the_child_no_input_by_default() {
    // `null`, so a Child that reads its input finds it empty rather than waiting on the host's.
    let rt = runtime();
    let output: String = eval(
        &rt,
        r#"return process.run(avrt([[io.write("[", io.read("a"), "]")]])).stdout:bytes()"#,
    );
    assert_eq!(output, "[]");
}

#[test]
fn run_feeds_a_string_as_input_and_closes_it() {
    let rt = runtime();
    let exact: bool = eval(
        &rt,
        r#"return process.run(avrt([[io.write(io.read("a"))]], { stdin = "fed\0\255" }))
             .stdout:bytes() == "fed\0\255""#,
    );
    assert!(exact);
}

#[test]
fn run_feeds_a_buffer_as_input() {
    let rt = runtime();
    let output: String = eval(
        &rt,
        r#"
        local first = process.run(avrt([[io.write("from a buffer")]]))
        local echo = avrt([[io.write(io.read("a"))]], { stdin = first.stdout })
        return process.run(echo).stdout:bytes()
        "#,
    );
    assert_eq!(output, "from a buffer");
}

#[test]
fn spawn_feeds_a_string_as_input_and_leaves_no_writer() {
    let rt = runtime();
    let (writer, output): (bool, String) = eval(
        &rt,
        r#"
        local child = process.spawn(avrt([[io.write(io.read("a"))]], { stdin = "fed" }))
        return child.stdin == nil, child:output().stdout:bytes()
        "#,
    );
    assert!(writer);
    assert_eq!(output, "fed");
}

// Timeouts.

#[test]
fn a_timeout_kills_and_raises_with_what_was_read() {
    let rt = runtime();
    let started = Instant::now();
    let (kind, stdout, ok, message): (String, String, bool, String) = eval(
        &rt,
        r#"
        local ok, err = pcall(process.run, avrt([[
          io.write("partial") io.flush()
          require("utils").spawn_timeout(function() end, 60000)
        ]], { timeout = 1500 }))
        return err.kind, err.output.stdout:bytes(), err.output.ok, tostring(err)
        "#,
    );
    assert!(started.elapsed() < Duration::from_secs(20));
    assert_eq!(kind, "timeout");
    assert_eq!(stdout, "partial");
    assert!(!ok);
    assert_eq!(message, format!("process: {AVRT} timed out after 1500 ms"));
}

#[test]
fn a_timeout_raises_without_check_too() {
    let rt = runtime();
    let kind: String = eval(
        &rt,
        r#"
        local ok, err = pcall(process.run,
          avrt([[require("utils").spawn_timeout(function() end, 60000)]], { timeout = 200 }))
        return err.kind
        "#,
    );
    assert_eq!(kind, "timeout");
}

#[test]
fn a_timeout_does_not_wait_for_a_grandchild_holding_stdout() {
    // The Child starts a program of its own that inherits its stdout and outlives it by far. Only
    // the Child is killed, and the pipe it shared stays open until the grandchild ends.
    let rt = runtime();
    let started = Instant::now();
    let (kind, stdout): (String, String) = eval(
        &rt,
        r#"
        local ok, err = pcall(process.run, avrt([[
          require("process").spawn({ os.getenv("AVRT"), "-e",
            [=[require("utils").spawn_timeout(function() end, 8000)]=], stdout = "inherit" })
          io.write("started") io.flush()
          require("utils").spawn_timeout(function() end, 60000)
        ]], { timeout = 1500, env = { AVRT = AVRT } }))
        return err.kind, err.output.stdout:bytes()
        "#,
    );
    assert_eq!(kind, "timeout");
    assert_eq!(stdout, "started");
    assert!(
        started.elapsed() < Duration::from_secs(6),
        "took {:?}, so it waited for the grandchild",
        started.elapsed()
    );
}

#[test]
fn a_timeout_that_is_not_a_positive_number_is_refused() {
    let rt = runtime();
    for timeout in ["0", "-1", r#""soon""#] {
        let source = format!(r#"process.run(avrt("", {{ timeout = {timeout} }}))"#);
        let message = error_of(&rt, &source);
        assert!(message.contains("timeout"), "{message}");
    }
}

// A spawned Child.

#[test]
fn lines_are_read_as_they_are_written() {
    let rt = runtime();
    let lines: Vec<String> = eval(
        &rt,
        r#"
        local child = process.spawn(avrt([[io.write("one\ntwo\r\n\nlast")]]))
        local lines = {}
        for line in child.stdout:lines() do lines[#lines + 1] = line end
        child:wait()
        return lines
        "#,
    );
    assert_eq!(lines, ["one", "two", "", "last"]);
}

#[test]
fn line_gives_one_line_at_a_time_and_nil_at_the_end() {
    let rt = runtime();
    let (first, second, end): (String, String, Option<String>) = eval(
        &rt,
        r#"
        local child = process.spawn(avrt([[io.write("a\nb\n")]]))
        return child.stdout:line(), child.stdout:line(), child.stdout:line()
        "#,
    );
    assert_eq!((first.as_str(), second.as_str(), end), ("a", "b", None));
}

#[test]
fn read_gives_chunks_and_then_nil() {
    let rt = runtime();
    let (all, end): (String, bool) = eval(
        &rt,
        r#"
        local child = process.spawn(avrt([[io.write(string.rep("x", 100000))]]))
        local chunks = {}
        while true do
          local chunk = child.stdout:read()
          if chunk == nil then break end
          assert(#chunk > 0 and #chunk <= 8192)
          chunks[#chunks + 1] = chunk
        end
        return table.concat(chunks), child.stdout:read() == nil
        "#,
    );
    assert_eq!(all.len(), 100_000);
    assert!(end);
}

#[test]
fn rest_gives_everything_remaining_as_a_buffer() {
    let rt = runtime();
    let (first, rest, length): (String, String, i64) = eval(
        &rt,
        r#"
        local child = process.spawn(avrt([[io.write("head\nand the rest\0")]]))
        local first = child.stdout:line()
        local rest = child.stdout:rest()
        return first, rest:bytes(), #rest
        "#,
    );
    assert_eq!(first, "head");
    assert_eq!(rest, "and the rest\0");
    assert_eq!(length, 13);
}

#[test]
fn what_is_written_to_stdin_reaches_the_child_once_closed() {
    let rt = runtime();
    let output: String = eval(
        &rt,
        r#"
        local child = process.spawn(avrt([[io.write(io.read("a"))]]))
        child.stdin:write("one ")
        child.stdin:write(process.run(avrt([[io.write("two")]])).stdout)
        child.stdin:close()
        return child.stdout:rest():bytes()
        "#,
    );
    assert_eq!(output, "one two");
}

#[test]
fn writing_after_close_is_an_error() {
    let rt = runtime();
    let message = error_of(
        &rt,
        r#"
        local child = process.spawn(avrt(""))
        child.stdin:close()
        child.stdin:write("late")
        "#,
    );
    assert!(message.contains("closed"), "{message}");
}

#[test]
fn a_stream_that_is_not_piped_is_nil() {
    let rt = runtime();
    let (stdin, stdout, stderr): (bool, bool, bool) = eval(
        &rt,
        r#"
        local child =
          process.spawn(avrt("", { stdin = "null", stdout = "null", stderr = "stdout" }))
        return child.stdin == nil, child.stdout == nil, child.stderr == nil
        "#,
    );
    assert!(stdin && stdout && stderr);
}

#[test]
fn spawn_pipes_every_stream_by_default() {
    let rt = runtime();
    let (stdin, stdout, stderr): (bool, bool, bool) = eval(
        &rt,
        r#"
        local child = process.spawn(avrt(""))
        return child.stdin ~= nil, child.stdout ~= nil, child.stderr ~= nil
        "#,
    );
    assert!(stdin && stdout && stderr);
}

#[test]
fn output_drains_more_than_a_pipe_holds_without_deadlock() {
    let rt = runtime();
    let (stdout, stderr, ok): (i64, i64, bool) = eval(
        &rt,
        r#"
        local child = process.spawn(avrt([[
          io.write(string.rep("o", 1 << 20)) io.stderr:write(string.rep("e", 1 << 20))
        ]]))
        local output = child:output()
        return #output.stdout, #output.stderr, output.ok
        "#,
    );
    assert_eq!((stdout, stderr), (1 << 20, 1 << 20));
    assert!(ok);
}

#[test]
fn wait_gives_how_the_child_exited() {
    let rt = runtime();
    let (ok, code): (bool, i64) = eval(
        &rt,
        r#"
        local status = process.spawn(avrt("os.exit(7)")):wait()
        return status.ok, status.code
        "#,
    );
    assert!(!ok);
    assert_eq!(code, 7);
}

#[test]
fn kill_ends_a_child() {
    let beat = Heartbeat::new();
    let rt = runtime();
    let (ok, code, signal): (bool, Option<i64>, Option<i64>) = eval(
        &rt,
        &format!(
            r#"
            local child = process.spawn({})
            child:kill()
            local status = child:wait()
            return status.ok, status.code, status.signal
            "#,
            beating(&beat)
        ),
    );
    assert!(!ok);
    if cfg!(unix) {
        assert_eq!((code, signal), (None, Some(9)));
    }
}

#[cfg(unix)]
#[test]
fn terminate_asks_a_child_to_end() {
    let beat = Heartbeat::new();
    let rt = runtime();
    let signal: Option<i64> = eval(
        &rt,
        &format!(
            r#"
            local child = process.spawn({})
            child:terminate()
            return child:wait().signal
            "#,
            beating(&beat)
        ),
    );
    assert_eq!(signal, Some(15));
}

#[test]
fn kill_and_terminate_after_exit_do_nothing() {
    let rt = runtime();
    let code: i64 = eval(
        &rt,
        r#"
        local child = process.spawn(avrt(""))
        child:wait()
        child:kill()
        child:terminate()
        return child:wait().code
        "#,
    );
    assert_eq!(code, 0);
}

#[test]
fn a_child_has_a_pid() {
    let rt = runtime();
    let pid: i64 = eval(
        &rt,
        r#"
        local child = process.spawn(avrt(""))
        local pid = child:pid()
        child:wait()
        assert(child:pid() == pid, "the pid outlives the Child")
        return pid
        "#,
    );
    assert!(pid > 0);
}

#[test]
fn two_tasks_reading_one_stream_at_once_is_an_error() {
    let rt = runtime();
    let (message, read): (String, String) = eval(
        &rt,
        r#"
        local child = process.spawn(avrt([[
          require("utils").spawn_timeout(function() io.write("late") end, 500)
        ]]))
        local first = utils.spawn_task(function() read = child.stdout:read() end)
        utils.spawn_timeout(function()
          local ok, err = pcall(child.stdout.read, child.stdout)
          message = tostring(err)
        end, 100):await()
        first:await()
        return message, read
        "#,
    );
    assert!(message.contains("already being read"), "{message}");
    assert_eq!(read, "late");
}

// A Child counts as a task.

#[test]
fn a_child_counts_as_a_task_until_it_exits() {
    let rt = runtime();
    rt.block_on(rt.exec(
        r#"process.spawn(avrt([[require("utils").spawn_timeout(function() end, 300)]]))"#,
        "=spawn",
    ))
    .unwrap();
    assert_eq!(rt.outstanding_tasks(), 1);
    rt.block_on(rt.wait_for_tasks()).unwrap();
    assert_eq!(rt.outstanding_tasks(), 0);
}

#[test]
fn a_child_being_fed_counts_as_one_task() {
    // More input than a pipe holds, which the Child does not read at first.
    let rt = runtime();
    rt.block_on(rt.exec(
        r#"process.spawn(avrt([[require("utils").spawn_timeout(function() io.read("a") end, 300)]],
             { stdin = ("x"):rep(1 << 20) }))"#,
        "=spawn",
    ))
    .unwrap();
    assert_eq!(rt.outstanding_tasks(), 1);
    rt.block_on(rt.wait_for_tasks()).unwrap();
}

#[test]
fn feeding_a_child_ends_when_it_exits_though_a_program_it_started_holds_its_input() {
    // Only the Child is managed (ADR 0016). This one starts a program that inherits its input,
    // never reads it and outlives it; the feed it would block is not waited for.
    let rt = runtime();
    let started = Instant::now();
    rt.block_on(rt.exec(
        r#"process.spawn(avrt([[
          require("process").spawn({ os.getenv("AVRT"), "-e",
            "require('utils').spawn_timeout(function() end, 5000)",
            stdin = "inherit", stdout = "null", stderr = "null" })
          os.exit(0)
        ]], { env = { AVRT = AVRT }, stdin = ("x"):rep(1 << 20) }))"#,
        "=spawn",
    ))
    .unwrap();
    rt.block_on(rt.wait_for_tasks()).unwrap();
    assert!(started.elapsed() < Duration::from_secs(3), "{:?}", started.elapsed());
}

#[test]
fn wait_for_tasks_waits_for_a_child_to_finish() {
    let dir = TempDir::new();
    let done = dir.path().join("done");
    let rt = runtime();
    rt.block_on(rt.exec(
        format!(
            r#"process.spawn(avrt([[
              require("utils").spawn_timeout(function()
                io.open(os.getenv("DONE"), "w"):close()
              end, 300)
            ]], {{ env = {{ DONE = {} }} }}))"#,
            path_string(&done)
        ),
        "=spawn",
    ))
    .unwrap();
    rt.block_on(rt.wait_for_tasks()).unwrap();
    assert!(done.exists());
}

#[test]
fn abort_tasks_kills_a_child() {
    let beat = Heartbeat::new();
    let rt = runtime();
    rt.block_on(rt.exec(format!("process.spawn({})", beating(&beat)), "=spawn"))
        .unwrap();
    assert!(beat.beats());
    assert_eq!(rt.abort_tasks().unwrap(), 1);
    assert!(beat.stops());
}

#[test]
fn dropping_the_runtime_kills_a_child() {
    let beat = Heartbeat::new();
    let rt = runtime();
    rt.block_on(rt.exec(format!("process.spawn({})", beating(&beat)), "=spawn"))
        .unwrap();
    assert!(beat.beats());
    drop(rt);
    assert!(beat.stops());
}

#[test]
fn losing_the_handle_does_not_kill_a_child() {
    let beat = Heartbeat::new();
    let rt = runtime();
    rt.block_on(rt.exec(
        format!("process.spawn({}) collectgarbage() collectgarbage()", beating(&beat)),
        "=spawn",
    ))
    .unwrap();
    assert!(beat.beats());
    assert_eq!(rt.outstanding_tasks(), 1);
    rt.abort_tasks().unwrap();
    assert!(beat.stops());
}

#[test]
fn an_aborted_task_kills_the_child_its_run_was_waiting_for() {
    let beat = Heartbeat::new();
    let rt = runtime();
    rt.block_on(rt.exec(
        format!("utils.spawn_task(function() process.run({}) end)", beating(&beat)),
        "=spawn",
    ))
    .unwrap();
    // Drive the task far enough to start the Child.
    rt.block_on(rt.exec("utils.spawn_timeout(function() end, 200):await()", "=drive"))
        .unwrap();
    assert!(beat.beats());
    rt.abort_tasks().unwrap();
    assert!(beat.stops());
}

#[test]
fn a_cancelled_run_kills_its_child() {
    // Cancelling drops the chunk's future where it waits, with the executor then left undriven, so
    // the Child has to die with `run` itself and not whenever its watching task next runs.
    let beat = Heartbeat::new();
    let cancel = CancelHandle::new();
    let rt = cancellable(&cancel);
    let result = std::thread::scope(|scope| {
        scope.spawn(|| {
            assert!(beat.beats());
            cancel.cancel();
        });
        rt.block_on(rt.exec(format!("process.run({})", beating(&beat)), "=run"))
    });
    match result.unwrap_err() {
        avarice_rt::Error::Lua(err) => assert!(avarice_rt::was_cancelled(&err), "{err}"),
        other => panic!("expected a Lua error, got {other:?}"),
    }
    assert!(beat.stops());
}

#[test]
fn a_cancelled_execution_an_embedder_drives_kills_the_child_it_was_waiting_for() {
    // Under `enter` the embedder drops the future, not the runtime, and cannot then collect what
    // it left: Lua refuses to run while cancelled. The Execution's end has to.
    let beat = Heartbeat::new();
    let cancel = CancelHandle::new();
    let rt = cancellable(&cancel);
    let execution = rt.enter().unwrap();
    let mut chunk = Box::pin(
        rt.load(format!("process.run({})", beating(&beat)), "=run")
            .exec_async(),
    );
    std::thread::scope(|scope| {
        scope.spawn(|| {
            assert!(beat.beats());
            cancel.cancel();
        });
        rt.block_on(async {
            while !cancel.is_cancelled() {
                let _ = tokio::time::timeout(Duration::from_millis(10), chunk.as_mut()).await;
            }
        });
    });
    drop(chunk);
    assert!(beat.beats(), "nothing has freed the Child yet");
    drop(execution);
    assert!(beat.stops());
}

/// A runtime prepared as [`runtime`]'s is, which `cancel` cancels.
fn cancellable(cancel: &CancelHandle) -> Runtime {
    prepared(
        Runtime::builder(Profile::Trusted)
            .cancel_handle(cancel.clone())
            .build()
            .unwrap(),
    )
}

// Printing.

#[test]
fn a_child_prints_its_program_pid_and_state_and_never_its_arguments() {
    let rt = runtime();
    let (running, exited, pid): (String, String, i64) = eval(
        &rt,
        r#"
        local child = process.spawn(avrt([[io.read("a") os.exit(4) -- SECRET]]))
        local running = tostring(child)
        child.stdin:close()
        child:wait()
        return running, tostring(child), child:pid()
        "#,
    );
    assert_eq!(running, format!("Child({AVRT}, pid {pid}, running)"));
    assert_eq!(exited, format!("Child({AVRT}, pid {pid}, exited 4)"));
}

#[test]
fn an_error_prints_its_message_and_never_the_arguments() {
    let rt = runtime();
    let messages: Vec<String> = eval(
        &rt,
        r#"
        local messages = {}
        for _, command in ipairs({
          { "avarice-rt-no-such-program", "SECRET" },
          avrt("os.exit(1) -- SECRET", { check = true }),
          avrt("require('utils').spawn_timeout(function() end, 60000) -- SECRET",
            { timeout = 200 }),
        }) do
          local ok, err = pcall(process.run, command)
          messages[#messages + 1] = tostring(err)
        end
        return messages
        "#,
    );
    assert_eq!(messages.len(), 3);
    for message in messages {
        assert!(message.starts_with("process: "), "{message}");
        assert!(!message.contains("SECRET"), "{message}");
    }
}

#[test]
fn an_uncaught_error_reaches_the_host_as_its_message() {
    let rt = runtime();
    let message = error_of(&rt, r#"process.run({ "avarice-rt-no-such-program" })"#);
    assert!(
        message.contains("could not start 'avarice-rt-no-such-program': program not found"),
        "{message}"
    );
}
