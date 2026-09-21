//! `print`, and the write sink it goes to.
//!
//! The sink is redirected to a buffer and read back, so nothing here spawns a process or captures
//! stdout. The one thing that has to be a process — how the default sink interleaves with `io`'s
//! buffering — is in `tests/cli.rs`.

mod common;

use std::io::{self, BufWriter, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use avarice_rt::mlua::StdLib;
use avarice_rt::{was_timed_out, Error, Profile, Runtime, DEFAULT_SANDBOX_MEMORY_LIMIT};
use common::Buffer;

/// A runtime whose `print` writes to the returned buffer.
fn printing(profile: Profile) -> (Runtime, Buffer) {
    let buffer = Buffer::new();
    let rt = Runtime::builder(profile)
        .write_sink(buffer.clone())
        .build()
        .unwrap();
    (rt, buffer)
}

/// Runs `source` on a fresh runtime and returns what it printed.
fn printed(profile: Profile, source: &str) -> String {
    let (rt, buffer) = printing(profile);
    rt.block_on(rt.exec(source, "=test")).unwrap();
    buffer.contents()
}

const BOTH: [Profile; 2] = [Profile::Sandbox, Profile::Trusted];

// -- Scalars -----------------------------------------------------------------------------------

#[test]
fn print_separates_arguments_with_tabs_and_ends_the_line() {
    for profile in BOTH {
        assert_eq!(
            printed(profile, r#"print(1, "x", nil, true)"#),
            "1\tx\tnil\ttrue\n"
        );
    }
}

#[test]
fn print_with_nothing_prints_an_empty_line() {
    assert_eq!(printed(Profile::Sandbox, "print()"), "\n");
}

#[test]
fn scalars_format_exactly_as_tostring_does() {
    // Compared against Lua's own `tostring` rather than against literals, so this stays true
    // whatever the platform does with `-0.0` or NaN.
    for profile in BOTH {
        let (rt, buffer) = printing(profile);
        let expected: String = rt
            .block_on(rt.eval(
                r#"
                local values = { 1, -1, 1.0, 0.5, 1e100, -0.0, 2^63, math.huge, -math.huge, 0/0,
                                 true, false, "text", "", "with\nnewline and \0 nul" }
                local expected = {}
                for i, v in ipairs(values) do
                  print(v)
                  expected[#expected + 1] = tostring(v)
                end
                return table.concat(expected, "\n") .. "\n"
                "#,
                "=test",
            ))
            .unwrap();
        assert_eq!(buffer.contents(), expected);
    }
}

#[test]
fn a_string_is_printed_raw_at_the_top_level_and_quoted_inside_a_table() {
    assert_eq!(printed(Profile::Sandbox, r#"print("a\nb")"#), "a\nb\n");
    assert_eq!(
        printed(Profile::Sandbox, r#"print({ "a\nb", 'say "hi"' })"#),
        "{\n  \"a\\nb\",\n  \"say \\\"hi\\\"\",\n}\n"
    );
}

#[test]
fn functions_and_other_non_tables_print_as_tostring_does() {
    let text = printed(Profile::Sandbox, "print(print, coroutine.create(print))");
    let mut parts = text.trim_end().split('\t');
    assert!(parts.next().unwrap().starts_with("function: "), "{text}");
    assert!(parts.next().unwrap().starts_with("thread: "), "{text}");
}

// -- Functions ---------------------------------------------------------------------------------

/// What follows the address in a printed function: `(a, b)` for a Lua function, `` for one
/// written in Rust. Fails if `text` is not one function followed by a newline.
fn after_address(text: &str) -> &str {
    let rest = text
        .strip_suffix('\n')
        .and_then(|line| line.strip_prefix("function: 0x"))
        .unwrap_or_else(|| panic!("not a function line: {text:?}"));
    let digits = rest.chars().take_while(char::is_ascii_hexdigit).count();
    assert!(digits > 0, "no address in {text:?}");
    &rest[digits..]
}

#[test]
fn a_lua_function_prints_its_parameters_after_its_address() {
    for profile in BOTH {
        let signature = |source: &str| printed(profile, source);
        assert_eq!(
            after_address(&signature("print(function(a, b) end)")),
            "(a, b)"
        );
        assert_eq!(after_address(&signature("print(function() end)")), "()");
        assert_eq!(
            after_address(&signature("print(function(...) end)")),
            "(...)"
        );
        assert_eq!(
            after_address(&signature("print(function(a, b, ...) end)")),
            "(a, b, ...)"
        );
    }
}

#[test]
fn a_method_shows_its_self() {
    let text = printed(
        Profile::Sandbox,
        "local o = {} function o:area(scale) end print(o.area)",
    );
    assert_eq!(after_address(&text), "(self, scale)");
}

#[test]
fn a_function_written_in_rust_prints_as_tostring_does() {
    // Nothing is known about its parameters, and `()` would claim it takes none.
    let text = printed(Profile::Sandbox, "print(string.format)");
    assert_eq!(after_address(&text), "");
}

#[test]
fn a_function_inside_a_table_prints_its_parameters_too() {
    let text = printed(
        Profile::Sandbox,
        "print({ add = function(a, b) end, format = string.format })",
    );
    let lines: Vec<_> = text.lines().collect();
    assert_eq!(lines.len(), 4, "{text}");
    let field = |line: &str, name: &str| {
        let value = line
            .trim()
            .strip_prefix(&format!("{name} = "))
            .and_then(|value| value.strip_suffix(','))
            .unwrap_or_else(|| panic!("not a `{name}` field: {line:?}"));
        after_address(&format!("{value}\n")).to_owned()
    };
    assert_eq!(field(lines[1], "add"), "(a, b)");
    assert_eq!(field(lines[2], "format"), "");
}

#[test]
fn tostring_is_not_changed() {
    // Only `print` shows parameters, so a script that parses or compares `tostring(f)` is
    // unaffected.
    let text = printed(
        Profile::Sandbox,
        "local f = function(a) end print(tostring(f) == string.format('function: %p', f))",
    );
    assert_eq!(text, "true\n");
}

#[test]
fn a_function_with_no_parameter_names_shows_question_marks() {
    // Stripped debug information is the only way names go missing, and only trusted mode can load
    // a binary chunk to have it happen.
    let text = printed(
        Profile::Trusted,
        r#"
        local stripped = load(string.dump(function(a, b, ...) end, true), "=stripped", "b")
        print(stripped)
        "#,
    );
    assert_eq!(after_address(&text), "(?, ?, ...)");
}

#[test]
fn a_stdlib_function_prints_the_names_its_lua_layer_gave_it() {
    let text = printed(Profile::Trusted, r#"print(require("validation").regex)"#);
    assert_eq!(after_address(&text), "(expression)");
}

#[test]
fn printing_a_function_works_from_inside_a_coroutine() {
    // The parameters are read off the Lua state `print` is running on, which in a coroutine is
    // not the main one.
    let text = printed(
        Profile::Sandbox,
        "coroutine.wrap(function() print(function(inside) end) end)()",
    );
    assert_eq!(after_address(&text), "(inside)");
}

#[test]
fn a_function_in_a_self_containing_table_leaves_the_cycle_mark_alone() {
    // Functions are leaves, so they cannot be what makes a table cyclic, and printing one must
    // not disturb the cycle mark of the table around it.
    let text = printed(
        Profile::Sandbox,
        "local t = { f = function(x) end } t.self = t print(t)",
    );
    assert!(text.contains("<cycle: table: 0x"), "{text}");
    assert!(text.contains("(x),"), "{text}");
}

// -- Tables ------------------------------------------------------------------------------------

#[test]
fn a_table_renders_structurally() {
    let expected = "\
{
  1,
  2,
  a = 1,
  b = {
    \"x\",
  },
  [\"with space\"] = true,
}
";
    for profile in BOTH {
        assert_eq!(
            printed(
                profile,
                r#"print({ 1, 2, a = 1, b = { "x" }, ["with space"] = true })"#
            ),
            expected
        );
    }
}

#[test]
fn an_empty_table_is_braces() {
    assert_eq!(printed(Profile::Sandbox, "print({})"), "{}\n");
    assert_eq!(printed(Profile::Sandbox, "print({ {} })"), "{\n  {},\n}\n");
}

#[test]
fn keys_come_out_in_a_fixed_order_whatever_the_table_was_built_in() {
    let expected = "\
{
  \"first\",
  [-1] = \"neg\",
  [2.5] = \"frac\",
  alpha = 1,
  beta = 2,
  [false] = 0,
  [true] = 1,
}
";
    let forward = r#"
        print({ "first", [2.5] = "frac", [-1] = "neg", alpha = 1, beta = 2, [true] = 1, [false] = 0 })
    "#;
    let backward = r#"
        local t = {}
        t[false] = 0 t[true] = 1 t.beta = 2 t.alpha = 1 t[-1] = "neg" t[2.5] = "frac" t[1] = "first"
        print(t)
    "#;
    assert_eq!(printed(Profile::Sandbox, forward), expected);
    assert_eq!(printed(Profile::Sandbox, backward), expected);
}

#[test]
fn a_key_that_is_not_an_identifier_is_bracketed() {
    let text = printed(
        Profile::Sandbox,
        r#"print({ ["end"] = 1, ["1x"] = 2, _ok = 3 })"#,
    );
    assert_eq!(
        text,
        "{\n  [\"1x\"] = 2,\n  _ok = 3,\n  [\"end\"] = 1,\n}\n"
    );
}

#[test]
fn a_table_that_contains_itself_is_marked_rather_than_followed() {
    let text = printed(
        Profile::Sandbox,
        "local t = { name = 'loop' } t.me = t t.list = { t } print(t)",
    );
    assert!(
        text.starts_with("{\n  list = {\n    <cycle: table: 0x"),
        "{text}"
    );
    assert!(text.contains("\n  me = <cycle: table: 0x"), "{text}");
    assert!(text.contains("name = \"loop\""), "{text}");
}

#[test]
fn a_table_that_appears_twice_without_a_cycle_is_printed_both_times() {
    let text = printed(
        Profile::Sandbox,
        r#"local shared = { 1 } print({ a = shared, b = shared })"#,
    );
    assert_eq!(text, "{\n  a = {\n    1,\n  },\n  b = {\n    1,\n  },\n}\n");
}

#[test]
fn a_table_that_says_how_to_print_itself_is_printed_that_way() {
    let text = printed(
        Profile::Sandbox,
        r#"
        local point = setmetatable({ x = 1 }, { __tostring = function() return "point!" end })
        print(point)
        print({ point })
        "#,
    );
    assert_eq!(text, "point!\n{\n  point!,\n}\n");
}

#[test]
fn a_table_with_a_protected_metatable_prints_as_stock_print_would() {
    // `__metatable` hides the real metatable from `getmetatable`, so the choice between the two
    // has to come from what `tostring` says.
    let text = printed(
        Profile::Sandbox,
        r#"
        local with = setmetatable({}, { __metatable = "locked", __tostring = function() return "custom" end })
        local without = setmetatable({ x = 1 }, { __metatable = "locked" })
        print(with)
        print(without)
        "#,
    );
    assert_eq!(text, "custom\n{\n  x = 1,\n}\n");
}

#[test]
fn a_tostring_metamethod_runs_once_per_table_printed() {
    let text = printed(
        Profile::Sandbox,
        r#"
        local calls = 0
        local counted = setmetatable({}, { __tostring = function() calls = calls + 1 return "c" end })
        print(counted)
        print({ counted })
        print(calls)
        "#,
    );
    assert_eq!(text, "c\n{\n  c,\n}\n2\n");
}

#[test]
fn printing_does_not_run_index_or_pairs_metamethods() {
    let text = printed(
        Profile::Sandbox,
        r#"
        local guarded = setmetatable({ real = 1 }, {
          __index = function() error("__index ran") end,
          __pairs = function() error("__pairs ran") end,
          __len = function() error("__len ran") end,
        })
        print(guarded)
        "#,
    );
    assert_eq!(text, "{\n  real = 1,\n}\n");
}

// -- Limits ------------------------------------------------------------------------------------

#[test]
fn the_adr_0005_case_a_thirty_thousand_deep_table_is_an_error_not_an_abort() {
    // This is the whole reason `print` is written in Lua rather than Rust (ADR 0005). Astra's
    // Rust pretty-printer recurses once per level and aborts the process on this input, well
    // inside the sandbox's memory cap; here it must be a Lua error a script can catch, with the
    // process still standing afterwards. Do not delete this test because it is slow.
    let (rt, buffer) = printing(Profile::Sandbox);
    assert_eq!(
        rt.memory_limit(),
        Some(DEFAULT_SANDBOX_MEMORY_LIMIT),
        "the case is about the sandbox's default cap"
    );

    let message: String = rt
        .block_on(rt.eval(
            r#"
            local t = {}
            for i = 1, 30000 do t = { t } end
            local ok, err = pcall(print, t)
            assert(not ok, "printing a 30 000-deep table should not succeed")
            return tostring(err)
            "#,
            "=test",
        ))
        .unwrap();
    assert!(
        message.contains("overflow") || message.contains("memory"),
        "{message}"
    );
    // Nothing is written until the whole value has been built.
    assert_eq!(buffer.contents(), "");

    // And the state is still good for more work.
    let sum: i64 = rt.block_on(rt.eval("return 1 + 1", "=after")).unwrap();
    assert_eq!(sum, 2);
    rt.block_on(rt.exec("print('still here')", "=after"))
        .unwrap();
    assert_eq!(buffer.contents(), "still here\n");
}

#[test]
fn a_time_limit_interrupts_a_tostring_that_never_returns() {
    let buffer = Buffer::new();
    let rt = Runtime::builder(Profile::Sandbox)
        .write_sink(buffer.clone())
        .time_limit(Duration::from_millis(100))
        .check_interval(1_000)
        .build()
        .unwrap();
    let err = rt
        .block_on(rt.exec(
            r#"
            print(setmetatable({}, { __tostring = function() while true do end end }))
            "#,
            "=test",
        ))
        .unwrap_err();
    let Error::Lua(err) = err else {
        panic!("expected a Lua error, got {err:?}")
    };
    assert!(was_timed_out(&err), "{err}");
    assert_eq!(buffer.contents(), "");
}

// -- The sink ----------------------------------------------------------------------------------

#[test]
fn the_sink_can_be_replaced_on_a_built_runtime() {
    let (rt, first) = printing(Profile::Sandbox);
    rt.block_on(rt.exec("print('one')", "=test")).unwrap();

    let second = Buffer::new();
    rt.set_write_sink(second.clone());
    rt.block_on(rt.exec("print('two')", "=test")).unwrap();

    assert_eq!(first.contents(), "one\n");
    assert_eq!(second.contents(), "two\n");
}

#[test]
fn a_print_that_was_kept_by_the_script_follows_the_replaced_sink() {
    let (rt, _) = printing(Profile::Sandbox);
    rt.block_on(rt.exec("kept = print", "=test")).unwrap();
    let second = Buffer::new();
    rt.set_write_sink(second.clone());
    rt.block_on(rt.exec("kept('later')", "=test")).unwrap();
    assert_eq!(second.contents(), "later\n");
}

#[test]
fn each_print_is_flushed_before_it_returns() {
    // A buffered sink would hold this back if `print` did not flush.
    let buffer = Buffer::new();
    let rt = Runtime::builder(Profile::Sandbox)
        .write_sink(BufWriter::with_capacity(1 << 16, buffer.clone()))
        .build()
        .unwrap();
    rt.block_on(rt.exec("print('a')", "=test")).unwrap();
    assert_eq!(buffer.contents(), "a\n");
    rt.block_on(rt.exec("print({ 1 })", "=test")).unwrap();
    assert_eq!(buffer.contents(), "a\n{\n  1,\n}\n");
}

#[test]
fn a_print_call_is_one_write_and_one_flush() {
    struct Counting {
        writes: Arc<AtomicUsize>,
        flushes: Arc<AtomicUsize>,
    }
    impl Write for Counting {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.writes.fetch_add(1, Ordering::SeqCst);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            self.flushes.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }
    let (writes, flushes) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let rt = Runtime::builder(Profile::Sandbox)
        .write_sink(Counting {
            writes: Arc::clone(&writes),
            flushes: Arc::clone(&flushes),
        })
        .build()
        .unwrap();
    rt.block_on(rt.exec("print(1, 2, 3) print({ a = { b = 1 } })", "=test"))
        .unwrap();
    assert_eq!(writes.load(Ordering::SeqCst), 2);
    assert_eq!(flushes.load(Ordering::SeqCst), 2);
}

#[test]
fn a_sink_that_fails_makes_print_raise_an_error_a_script_can_catch() {
    struct Broken;
    impl Write for Broken {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "the pipe is gone",
            ))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let rt = Runtime::builder(Profile::Sandbox)
        .write_sink(Broken)
        .build()
        .unwrap();
    let message: String = rt
        .block_on(rt.eval(
            "local ok, err = pcall(print, 1) assert(not ok) return tostring(err)",
            "=test",
        ))
        .unwrap();
    assert!(message.contains("could not write output"), "{message}");
    assert!(message.contains("the pipe is gone"), "{message}");
}

#[test]
fn a_script_can_replace_print_as_it_can_replace_any_global() {
    let (rt, buffer) = printing(Profile::Sandbox);
    rt.block_on(rt.exec("print = function() end print('swallowed')", "=test"))
        .unwrap();
    assert_eq!(buffer.contents(), "");
}

#[test]
fn a_script_redefining_tostring_does_not_change_how_print_formats() {
    let text = printed(
        Profile::Sandbox,
        "tostring = function() return 'hijacked' end print(1, { 2 })",
    );
    assert_eq!(text, "1\t{\n  2,\n}\n");
}

#[test]
fn a_runtime_without_the_string_or_table_library_still_prints() {
    let buffer = Buffer::new();
    let rt = Runtime::builder(Profile::Sandbox)
        .std_libs(StdLib::NONE)
        .write_sink(buffer.clone())
        .build()
        .unwrap();
    rt.block_on(rt.exec("print(1, 'two', true, nil)", "=test"))
        .unwrap();
    assert_eq!(buffer.contents(), "1\ttwo\ttrue\tnil\n");

    // Tables fall back to what stock `print` shows.
    rt.block_on(rt.exec("print({})", "=test")).unwrap();
    assert!(
        buffer.contents().contains("table: 0x"),
        "{}",
        buffer.contents()
    );
}
