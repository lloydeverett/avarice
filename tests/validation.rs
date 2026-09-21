//! The `validation` module: Astra's schema validators and its regular expressions, reached
//! through `require`.
//!
//! The module's Lua is Astra's, so these tests are about what could go wrong on the way in: that
//! it loads and runs here, that the regex primitive is there when this is the only module, and
//! that a file which defines its functions as globals does not leak them into the runtime.

#![cfg(feature = "stdlib-validation")]

use avarice_rt::mlua::FromLuaMulti;
use avarice_rt::{Profile, Runtime, StdModules};

fn trusted() -> Runtime {
    Runtime::new(Profile::Trusted).unwrap()
}

fn run<R: FromLuaMulti>(rt: &Runtime, code: &str) -> R {
    rt.block_on(rt.eval(code, "=test")).unwrap()
}

// -- Regular expressions -----------------------------------------------------------------------

#[test]
fn a_regex_says_whether_it_matches() {
    let rt = trusted();
    let (yes, no): (bool, bool) = run(
        &rt,
        r#"
        local re = require("validation").regex("^a+b$")
        return re:is_match("aaab"), re:is_match("abb")
        "#,
    );
    assert!(yes);
    assert!(!no);
}

#[test]
fn captures_come_back_one_list_per_match() {
    let rt = trusted();
    let flat: String = run(
        &rt,
        r#"
        local found = require("validation").regex("(a+)(b)"):captures("aab ab")
        local out = {}
        for _, groups in ipairs(found) do out[#out + 1] = table.concat(groups, ",") end
        return table.concat(out, ";")
        "#,
    );
    // Each list is the whole match, then the groups.
    assert_eq!(flat, "aab,aa,b;ab,a,b");
}

#[test]
fn replace_takes_a_limit_and_groups() {
    let rt = trusted();
    let (all, first): (String, String) = run(
        &rt,
        r#"
        local re = require("validation").regex("(a+)b")
        return re:replace("aab ab", "<$1>"), re:replace("aab ab", "<$1>", 1)
        "#,
    );
    assert_eq!(all, "<aa> <a>");
    assert_eq!(first, "<aa> ab");
}

#[test]
fn a_bad_pattern_is_an_error_the_script_can_catch() {
    let rt = trusted();
    let message: String = run(
        &rt,
        r#"
        local ok, err = pcall(require("validation").regex, "(")
        return tostring(ok) .. ": " .. tostring(err)
        "#,
    );
    assert!(message.starts_with("false: "), "{message}");
    assert!(message.contains("Could not compile the regex"), "{message}");
}

#[test]
fn regex_works_when_validation_is_the_only_module() {
    // The regex primitive belongs to Astra's `utils` Rust half, so `validation` has to bring it
    // itself rather than lean on `utils` having been loaded.
    let rt = Runtime::builder(Profile::Sandbox)
        .with_std_modules(StdModules::VALIDATION)
        .build()
        .unwrap();
    assert!(!rt.has_module("utils"));
    let matched: bool = run(
        &rt,
        r#"return require("validation").regex("^x$"):is_match("x")"#,
    );
    assert!(matched);
}

// -- Validators --------------------------------------------------------------------------------

/// Runs `body` with `t` bound to `require("validation").types`.
fn with_types<R: FromLuaMulti>(body: &str) -> R {
    let rt = trusted();
    run(
        &rt,
        &format!(r#"local t = require("validation").types {body}"#),
    )
}

#[test]
fn a_struct_accepts_what_fits_and_says_where_it_does_not() {
    let (ok, fine_err, bad, bad_err): (bool, Option<String>, bool, String) = with_types(
        r#"
        local user = t.struct({ name = t.string(), age = t.integer() })
        local ok, fine_err = user:validate({ name = "ada", age = 36 })
        local bad, bad_err = user:validate({ name = "ada", age = 36.5 })
        return ok, fine_err, bad, bad_err
        "#,
    );
    assert!(ok);
    assert_eq!(fine_err, None);
    assert!(!bad);
    assert_eq!(bad_err, "age: expected integer, got 36.5");
}

#[test]
fn a_struct_refuses_keys_it_does_not_know() {
    let err: String = with_types(
        r#"
        local _, err = t.struct({ a = t.number() }):validate({ a = 1, extra = true })
        return err
        "#,
    );
    assert_eq!(err, "unexpected key: extra");
}

#[test]
fn errors_name_the_path_through_nested_values() {
    let err: String = with_types(
        r#"
        local schema = t.struct({ tags = t.array(t.struct({ id = t.number() })) })
        local _, err = schema:validate({ tags = { { id = 1 }, { id = "two" } } })
        return err
        "#,
    );
    assert_eq!(err, "tags.[2].id: expected number, got string");
}

#[test]
fn ranges_patterns_literals_and_unions_check_their_values() {
    // What each reports as well as whether it passes, since a script shows the message to a user.
    let outcomes: Vec<String> = {
        let rt = trusted();
        let table: avarice_rt::mlua::Table = run(
            &rt,
            r#"
            local t = require("validation").types
            local function say(validator, value)
              local ok, err = validator:validate(value)
              return ok and "ok" or err
            end
            local age = t.range({ min = 0, max = 150 })
            local slug = t.pattern("^[a-z-]+$")
            local kind = t.literal("cat")
            local either = t.union(t.number(), t.boolean())
            return {
              say(age, 30), say(age, 200),
              say(slug, "a-b"), say(slug, "A B"),
              say(kind, "cat"), say(kind, "dog"),
              say(either, 1), say(either, true), say(either, "x"),
            }
            "#,
        );
        table.sequence_values().collect::<Result<_, _>>().unwrap()
    };
    assert_eq!(
        outcomes,
        [
            "ok",
            "out of range",
            "ok",
            "string does not match pattern",
            "ok",
            "expected cat, got dog",
            "ok",
            "ok",
            "value did not match any union member",
        ]
    );
}

#[test]
fn booleans_and_nil_are_checked_too() {
    let (yes, no, nothing, something): (bool, String, bool, String) = with_types(
        r#"
        local flag, none = t.boolean(), t.none()
        local _, not_a_flag = flag:validate("yes")
        local _, not_nothing = none:validate(1)
        return (flag:validate(true)), not_a_flag, (none:validate(nil)), not_nothing
        "#,
    );
    assert!(yes);
    assert_eq!(no, "expected boolean, got string");
    assert!(nothing);
    assert_eq!(something, "expected nil, got number");
}

#[test]
fn an_optional_field_may_be_absent_but_not_wrong() {
    let (absent, wrong): (bool, bool) = with_types(
        r#"
        local schema = t.struct({ nick = t.optional(t.string()) })
        return (schema:validate({})), (schema:validate({ nick = 5 }))
        "#,
    );
    assert!(absent);
    assert!(!wrong);
}

#[test]
fn build_fills_defaults_and_refuses_what_does_not_validate() {
    let (role, name, refused): (String, String, String) = with_types(
        r#"
        local User = t.build(t.struct({
          name = t.string(),
          role = t.string({ default = "user" }),
        }))
        local made = User({ name = "ada" })
        local _, err = pcall(User, { role = "admin" })
        return made.role, made.name, tostring(err)
        "#,
    );
    assert_eq!(role, "user");
    assert_eq!(name, "ada");
    assert_eq!(refused, "name: expected string, got nil");
}

#[test]
fn an_optional_field_can_carry_a_default_that_build_fills() {
    let (filled, kept): (i64, i64) = with_types(
        r#"
        local Sized = t.build(t.struct({ size = t.optional(t.number({ default = 10 })) }))
        return Sized({}).size, Sized({ size = 3 }).size
        "#,
    );
    assert_eq!(filled, 10);
    assert_eq!(kept, 3);
}

// -- Not leaking into the runtime --------------------------------------------------------------

#[test]
fn requiring_it_adds_no_globals_beyond_the_primitive() {
    // Astra's file declares `number`, `struct`, `regex` and a dozen more as globals. Left alone
    // they would appear in every Lua program the moment anything required the module.
    let rt = trusted();
    let added: String = run(
        &rt,
        r#"
        local before = {}
        for name in pairs(_G) do before[name] = true end
        require("validation")
        local added = {}
        for name in pairs(_G) do
          if not before[name] and not name:find("^astra_internal__") then
            added[#added + 1] = name
          end
        end
        table.sort(added)
        return table.concat(added, ",")
        "#,
    );
    assert_eq!(added, "");
}

#[test]
fn a_scripts_own_globals_neither_break_it_nor_are_broken_by_it() {
    // `integer` calls `number` by its global name, so if the module's functions lived in `_G` a
    // script that reused that name would break the module, and the module would break the script.
    let rt = trusted();
    let (integer_ok, number, struct_): (bool, String, String) = run(
        &rt,
        r#"
        number = "mine"
        struct = "also mine"
        local t = require("validation").types
        return (t.integer():validate(3)), number, struct
        "#,
    );
    assert!(integer_ok);
    assert_eq!(number, "mine");
    assert_eq!(struct_, "also mine");
}
