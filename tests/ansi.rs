//! The `ansi` core module, reached through `require`.
//!
//! It is pure Lua and registered in every runtime whatever its features (ADR 0011), so every test
//! also holds in a sandbox, and the ones that matter for that say so. Each test drives
//! `require("ansi")` and looks at the strings it hands back, which are what a terminal is given.

mod common;

use avarice::{Profile, Runtime, StdModules};
use common::{listed, requirable};

fn runtime() -> Runtime {
    Runtime::new(Profile::Sandbox).unwrap()
}

/// Evaluates `expr` with `ansi` in scope, as a string.
fn string(rt: &Runtime, expr: &str) -> String {
    rt.block_on(rt.eval::<String>(
        &format!("local ansi = require('ansi') return {expr}"),
        "=test",
    ))
    .unwrap()
}

/// The message of the error `expr` raises, with `ansi` in scope.
fn error(rt: &Runtime, expr: &str) -> String {
    rt.block_on(rt.eval::<String>(
        &format!(
            "local ansi = require('ansi') local ok, err = pcall(function() return {expr} end) \
             assert(not ok, 'expected an error') return tostring(err)"
        ),
        "=test",
    ))
    .unwrap()
}

// -- Availability ------------------------------------------------------------------------------

#[test]
fn a_sandbox_has_ansi() {
    assert!(requirable(&runtime(), "ansi"));
}

#[test]
fn trusted_mode_has_ansi_too() {
    assert!(requirable(&Runtime::new(Profile::Trusted).unwrap(), "ansi"));
}

#[test]
fn ansi_is_core_so_no_selection_of_stdlib_modules_removes_it() {
    // It is registered by the core, and no build or profile decides it (ADR 0011).
    for profile in [Profile::Sandbox, Profile::Trusted] {
        let rt = Runtime::builder(profile)
            .std_modules(StdModules::NONE)
            .build()
            .unwrap();
        assert!(rt.has_module("ansi"), "{profile:?}");
        assert!(requirable(&rt, "ansi"), "{profile:?}");
    }
}

#[test]
fn ansi_is_not_a_stdlib_module_so_stdlib_does_not_list_it() {
    for profile in [Profile::Sandbox, Profile::Trusted] {
        let rt = Runtime::new(profile).unwrap();
        assert!(!listed(&rt).contains(&"ansi".to_owned()), "{profile:?}");
    }
}

// -- Styles ------------------------------------------------------------------------------------

#[test]
fn styles_are_complete_escape_sequences() {
    let rt = runtime();
    for (name, code) in [
        ("reset", 0),
        ("bold", 1),
        ("dim", 2),
        ("italic", 3),
        ("underline", 4),
        ("blink", 5),
        ("reverse", 7),
        ("hidden", 8),
        ("strikethrough", 9),
    ] {
        assert_eq!(
            string(&rt, &format!("ansi.{name}")),
            format!("\x1b[{code}m")
        );
    }
}

#[test]
fn every_style_but_reset_has_an_off_code() {
    let rt = runtime();
    for (name, code) in [
        ("no_bold", 22),
        ("no_dim", 22),
        ("no_italic", 23),
        ("no_underline", 24),
        ("no_blink", 25),
        ("no_reverse", 27),
        ("no_hidden", 28),
        ("no_strikethrough", 29),
    ] {
        assert_eq!(
            string(&rt, &format!("ansi.{name}")),
            format!("\x1b[{code}m")
        );
    }
}

#[test]
fn bold_and_dim_share_an_off_code_because_the_terminal_has_one() {
    let rt = runtime();
    assert_eq!(string(&rt, "ansi.no_bold"), string(&rt, "ansi.no_dim"));
}

// -- Colours -----------------------------------------------------------------------------------

const COLOURS: [&str; 8] = [
    "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
];

#[test]
fn foreground_colours_are_the_thirties_and_the_nineties() {
    let rt = runtime();
    for (offset, colour) in COLOURS.into_iter().enumerate() {
        assert_eq!(
            string(&rt, &format!("ansi.fg.{colour}")),
            format!("\x1b[{}m", 30 + offset)
        );
        assert_eq!(
            string(&rt, &format!("ansi.fg.bright_{colour}")),
            format!("\x1b[{}m", 90 + offset)
        );
    }
    assert_eq!(string(&rt, "ansi.fg.default"), "\x1b[39m");
}

#[test]
fn background_colours_are_the_forties_and_the_hundreds() {
    let rt = runtime();
    for (offset, colour) in COLOURS.into_iter().enumerate() {
        assert_eq!(
            string(&rt, &format!("ansi.bg.{colour}")),
            format!("\x1b[{}m", 40 + offset)
        );
        assert_eq!(
            string(&rt, &format!("ansi.bg.bright_{colour}")),
            format!("\x1b[{}m", 100 + offset)
        );
    }
    assert_eq!(string(&rt, "ansi.bg.default"), "\x1b[49m");
}

#[test]
fn codes_concatenate_with_text() {
    let rt = runtime();
    assert_eq!(
        string(
            &rt,
            r#"ansi.bold .. ansi.fg.red .. "error" .. ansi.reset .. ": disk full""#
        ),
        "\x1b[1m\x1b[31merror\x1b[0m: disk full"
    );
}

#[test]
fn an_unknown_name_is_nil_and_not_an_error() {
    let rt = runtime();
    for expr in ["ansi.sparkle", "ansi.fg.chartreuse", "ansi.bg.chartreuse"] {
        let absent: bool = rt
            .block_on(rt.eval(
                &format!("local ansi = require('ansi') return ({expr}) == nil"),
                "=test",
            ))
            .unwrap();
        assert!(absent, "{expr}");
    }
}

// -- True colour and the 256-colour palette ----------------------------------------------------

#[test]
fn rgb_is_a_true_colour_sequence() {
    let rt = runtime();
    assert_eq!(
        string(&rt, "ansi.fg.rgb(255, 128, 0)"),
        "\x1b[38;2;255;128;0m"
    );
    assert_eq!(
        string(&rt, "ansi.bg.rgb(0, 51, 102)"),
        "\x1b[48;2;0;51;102m"
    );
    assert_eq!(string(&rt, "ansi.fg.rgb(0, 0, 0)"), "\x1b[38;2;0;0;0m");
    assert_eq!(
        string(&rt, "ansi.fg.rgb(255, 255, 255)"),
        "\x1b[38;2;255;255;255m"
    );
}

#[test]
fn color256_is_a_palette_sequence() {
    let rt = runtime();
    assert_eq!(string(&rt, "ansi.fg.color256(202)"), "\x1b[38;5;202m");
    assert_eq!(string(&rt, "ansi.bg.color256(202)"), "\x1b[48;5;202m");
    assert_eq!(string(&rt, "ansi.fg.color256(0)"), "\x1b[38;5;0m");
    assert_eq!(string(&rt, "ansi.fg.color256(255)"), "\x1b[38;5;255m");
}

#[test]
fn a_float_with_an_integer_value_is_accepted_as_lua_does_everywhere() {
    let rt = runtime();
    assert_eq!(
        string(&rt, "ansi.fg.rgb(255.0, 128, 0)"),
        "\x1b[38;2;255;128;0m"
    );
    assert_eq!(string(&rt, "ansi.fg.color256(202.0)"), "\x1b[38;5;202m");
}

#[test]
fn hex_takes_six_digits_with_or_without_a_hash_in_either_case() {
    let rt = runtime();
    for spelling in ["#ff8000", "ff8000", "#FF8000", "Ff8000"] {
        assert_eq!(
            string(&rt, &format!("ansi.fg.hex('{spelling}')")),
            "\x1b[38;2;255;128;0m",
            "{spelling}"
        );
    }
    assert_eq!(string(&rt, "ansi.bg.hex('#003366')"), "\x1b[48;2;0;51;102m");
}

// -- Arguments that are wrong are errors -------------------------------------------------------

#[test]
fn rgb_names_the_component_that_is_wrong() {
    let rt = runtime();
    for (call, component) in [
        ("ansi.fg.rgb(256, 0, 0)", "red"),
        ("ansi.fg.rgb(0, -1, 0)", "green"),
        ("ansi.bg.rgb(0, 0, 1.5)", "blue"),
        ("ansi.fg.rgb('1', 0, 0)", "red"),
        ("ansi.fg.rgb(0, nil, 0)", "green"),
        ("ansi.fg.rgb(0, 0)", "blue"),
    ] {
        let message = error(&rt, call);
        assert!(
            message.contains(&format!(
                "rgb: {component} component must be an integer 0–255"
            )),
            "{call}: {message}"
        );
    }
}

#[test]
fn color256_refuses_what_is_not_a_palette_index() {
    let rt = runtime();
    for call in [
        "ansi.fg.color256(256)",
        "ansi.bg.color256(-1)",
        "ansi.fg.color256(1.5)",
        "ansi.fg.color256('202')",
        "ansi.fg.color256()",
        "ansi.fg.color256(0/0)",
    ] {
        let message = error(&rt, call);
        assert!(
            message.contains("color256: n must be an integer 0–255"),
            "{call}: {message}"
        );
    }
}

#[test]
fn hex_refuses_anything_that_is_not_six_hex_digits() {
    let rt = runtime();
    for call in [
        "ansi.fg.hex('#f80')",
        "ansi.fg.hex('#ff80000')",
        "ansi.fg.hex('#gg8000')",
        "ansi.fg.hex('##ff8000')",
        "ansi.fg.hex(' ff8000')",
        "ansi.fg.hex('')",
        "ansi.bg.hex(0xff8000)",
        "ansi.bg.hex()",
    ] {
        let message = error(&rt, call);
        assert!(
            message.contains("hex: expected six hex digits, optionally after a #"),
            "{call}: {message}"
        );
    }
}

#[test]
fn an_error_points_at_the_caller_and_not_at_the_module() {
    let rt = runtime();
    // Each is called from a line of its own that is not a tail call, so the caller's frame is
    // the one an error is blamed on. `rgb` and `color256` validate one call deeper than `hex`
    // does, so the level is worth holding for each.
    for call in [
        "ansi.fg.rgb(256, 0, 0)",
        "ansi.bg.color256(256)",
        "ansi.fg.hex('#f80')",
    ] {
        let message = rt
            .block_on(rt.eval::<String>(
                &format!(
                    "local ansi = require('ansi')\n\
                     local _, err = pcall(function() local code = {call} return code end)\n\
                     return err"
                ),
                "=caller",
            ))
            .unwrap();
        assert!(message.starts_with("caller:2:"), "{call}: {message}");
    }
}

// -- Plain tables -------------------------------------------------------------------------------

#[test]
fn the_tables_are_plain_tables_that_a_program_may_write_to() {
    let rt = runtime();
    let plain: bool = rt
        .block_on(rt.eval(
            r#"
            local ansi = require("ansi")
            ansi.fg.red = "mine"
            ansi.custom = "\27[1;4m"
            return getmetatable(ansi) == nil
               and getmetatable(ansi.fg) == nil
               and getmetatable(ansi.bg) == nil
               and ansi.fg.red == "mine"
               and ansi.custom == "\27[1;4m"
            "#,
            "=test",
        ))
        .unwrap();
    assert!(plain);
}

// -- Pure ---------------------------------------------------------------------------------------

/// The names in the globals table, sorted.
fn globals(rt: &Runtime) -> Vec<String> {
    rt.block_on(rt.eval(
        "local t = {} for k in pairs(_G) do t[#t + 1] = tostring(k) end table.sort(t) return t",
        "=test",
    ))
    .unwrap()
}

#[test]
fn building_it_defines_no_global_and_sets_no_primitive() {
    let rt = runtime();
    let before = globals(&rt);
    assert!(requirable(&rt, "ansi"));
    assert_eq!(before, globals(&rt));
}
