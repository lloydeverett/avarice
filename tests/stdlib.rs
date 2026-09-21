//! The stdlib modules, reached through `require`, and the selection of which a runtime has.
//!
//! Every test drives `require` and calls what comes back. None calls
//! `avarice_rt_stdlib::loader` directly, because an embedder cannot: a module that works when
//! called directly but is not reachable through `require` is broken in the only way that matters.

mod common;

use avarice_rt::{FsStore, Profile, Runtime, StdModules};
use common::TempDir;

const NAMES: [&str; 8] = [
    "http",
    "fs",
    "crypto",
    "serde",
    "datetime",
    "utils",
    "stores",
    "validation",
];

/// Whether `require(name)` succeeds in `rt`.
fn requirable(rt: &Runtime, name: &str) -> bool {
    rt.block_on(rt.eval::<bool>(format!("return (pcall(require, '{name}'))"), "=test"))
        .unwrap()
}

/// The names of `NAMES` that `rt` can `require`.
fn reachable(rt: &Runtime) -> Vec<&'static str> {
    NAMES
        .into_iter()
        .filter(|name| requirable(rt, name))
        .collect()
}

// -- Selection ---------------------------------------------------------------------------------

#[test]
fn trusted_mode_registers_every_stdlib_module() {
    let rt = Runtime::new(Profile::Trusted).unwrap();
    assert_eq!(reachable(&rt), NAMES);
    for name in NAMES {
        assert!(rt.has_module(name), "{name}");
    }
}

#[test]
fn sandbox_mode_registers_none_and_says_module_not_found() {
    let rt = Runtime::new(Profile::Sandbox).unwrap();
    assert!(reachable(&rt).is_empty());
    for name in NAMES {
        assert!(!rt.has_module(name), "{name}");
        let message: String = rt
            .block_on(rt.eval(
                format!("local ok, err = pcall(require, '{name}') return tostring(err)"),
                "=test",
            ))
            .unwrap();
        assert!(
            message.contains(&format!("module '{name}' not found")),
            "{message}"
        );
    }
}

#[test]
fn trusted_minus_one_module_keeps_the_others() {
    let rt = Runtime::builder(Profile::Trusted)
        .without_std_modules(StdModules::HTTP)
        .build()
        .unwrap();
    assert!(!rt.has_module("http"));
    let expected: Vec<_> = NAMES.into_iter().filter(|name| *name != "http").collect();
    assert_eq!(reachable(&rt), expected);
}

#[test]
fn validation_can_be_taken_from_trusted_mode_and_added_to_a_sandbox() {
    let without = Runtime::builder(Profile::Trusted)
        .without_std_modules(StdModules::VALIDATION)
        .build()
        .unwrap();
    assert!(!without.has_module("validation"));
    assert!(!reachable(&without).contains(&"validation"));

    let with = Runtime::builder(Profile::Sandbox)
        .with_std_modules(StdModules::VALIDATION)
        .build()
        .unwrap();
    assert_eq!(reachable(&with), ["validation"]);
}

#[test]
fn sandbox_plus_one_module_has_that_one_and_no_other() {
    let rt = Runtime::builder(Profile::Sandbox)
        .with_std_modules(StdModules::FS)
        .build()
        .unwrap();
    assert!(rt.has_module("fs"));
    assert_eq!(reachable(&rt), ["fs"]);
}

#[test]
fn std_modules_replaces_the_set() {
    let rt = Runtime::builder(Profile::Trusted)
        .std_modules(StdModules::CRYPTO | StdModules::STORES)
        .build()
        .unwrap();
    assert_eq!(reachable(&rt), ["crypto", "stores"]);
}

#[test]
fn the_profile_is_unchanged_by_reconfiguring_one_runtime() {
    let _ = Runtime::builder(Profile::Trusted)
        .std_modules(StdModules::NONE)
        .build()
        .unwrap();
    let rt = Runtime::new(Profile::Trusted).unwrap();
    assert_eq!(reachable(&rt), NAMES);
}

#[test]
fn a_stdlib_module_is_not_built_until_it_is_required() {
    let rt = Runtime::new(Profile::Trusted).unwrap();
    // `crypto`'s Rust half sets its primitives as globals when the module is built, so their
    // absence is what "not built" looks like from Lua.
    let before: bool = rt
        .block_on(rt.eval("return astra_internal__hash == nil", "=test"))
        .unwrap();
    assert!(before, "crypto was built before it was required");
    assert!(rt.has_module("crypto"));

    let after: bool = rt
        .block_on(rt.eval(
            "require('crypto') return astra_internal__hash ~= nil",
            "=test",
        ))
        .unwrap();
    assert!(after, "requiring crypto did not build it");
}

#[test]
fn a_stdlib_module_is_built_once() {
    let rt = Runtime::new(Profile::Trusted).unwrap();
    let same: bool = rt
        .block_on(rt.eval("return require('stores') == require('stores')", "=test"))
        .unwrap();
    assert!(same);
}

#[test]
fn a_stdlib_name_shadows_a_store_module_of_that_name() {
    let dir = TempDir::new();
    dir.write("crypto.lua", "return { impostor = true }");
    dir.write("extra.lua", "return { present = true }");
    let rt = Runtime::builder(Profile::Trusted)
        .store(FsStore::new(dir.path()))
        .build()
        .unwrap();

    let impostor: bool = rt
        .block_on(rt.eval("return require('crypto').impostor == true", "=test"))
        .unwrap();
    assert!(!impostor, "the store's crypto won over the stdlib's");
    let hash_is_there: bool = rt
        .block_on(rt.eval("return type(require('crypto').hash) == 'function'", "=test"))
        .unwrap();
    assert!(hash_is_there);

    // Only the names the stdlib claims are shadowed.
    let other: bool = rt
        .block_on(rt.eval("return require('extra').present", "=test"))
        .unwrap();
    assert!(other);
}

#[test]
fn a_module_that_is_not_registered_leaves_the_stores_module_reachable() {
    let dir = TempDir::new();
    dir.write("crypto.lua", "return { impostor = true }");
    let rt = Runtime::builder(Profile::Trusted)
        .without_std_modules(StdModules::CRYPTO)
        .store(FsStore::new(dir.path()))
        .build()
        .unwrap();
    let impostor: bool = rt
        .block_on(rt.eval("return require('crypto').impostor == true", "=test"))
        .unwrap();
    assert!(impostor);
}

// -- stores ------------------------------------------------------------------------------------

#[test]
fn pubsub_delivers_to_subscribers_of_a_topic() {
    let rt = Runtime::new(Profile::Trusted).unwrap();
    let got: String = rt
        .block_on(rt.eval(
            r#"
            local stores = require("stores")
            local got = {}
            stores.pubsub.subscribe("greetings", function(data, topic)
              got[#got + 1] = topic .. ":" .. data
            end)
            stores.pubsub.publish("greetings", "hello")
            stores.pubsub.publish("elsewhere", "ignored")
            return table.concat(got, ",")
            "#,
            "=test",
        ))
        .unwrap();
    assert_eq!(got, "greetings:hello");
}

#[test]
fn pubsub_subscribe_takes_a_topic_and_a_callback() {
    // Astra's documentation describes a three-argument form, with an observable in the middle.
    // Its code implements two arguments, and so does ours.
    let rt = Runtime::new(Profile::Trusted).unwrap();
    let got: i64 = rt
        .block_on(rt.eval(
            r#"
            local pubsub = require("stores").pubsub
            local total = 0
            local function add(n) total = total + n end
            pubsub.subscribe("n", add)
            pubsub.publish("n", 5)
            pubsub.unsubscribe("n", add)
            pubsub.publish("n", 100)
            return total
            "#,
            "=test",
        ))
        .unwrap();
    assert_eq!(got, 5);
}

#[test]
fn an_observable_notifies_its_observers() {
    let rt = Runtime::new(Profile::Trusted).unwrap();
    let got: String = rt
        .block_on(rt.eval(
            r#"
            local observable = require("stores").observable(1)
            local seen = {}
            local function observe(v) seen[#seen + 1] = v end
            observable:subscribe(observe)
            observable:publish("a")
            observable:unsubscribe(observe)
            observable:publish("b")
            return table.concat(seen, ",")
            "#,
            "=test",
        ))
        .unwrap();
    assert_eq!(got, "a");
}

// -- crypto ------------------------------------------------------------------------------------

#[test]
fn hashes_match_the_published_vectors_for_abc() {
    // FIPS 180-4 and FIPS 202 example values for the message "abc".
    let rt = Runtime::new(Profile::Trusted).unwrap();
    let vectors = [
        (
            "sha2_256",
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        ),
        (
            "sha2_512",
            "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a\
             2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f",
        ),
        (
            "sha3_256",
            "3a985da74fe225b2045c172d6bd390bd855f086e3e9d525b46bfe24511431532",
        ),
        (
            "sha3_512",
            "b751850b1a57168a5693cd924b6b096e08f621827444f70d884f5d0240d2712e\
             10e116e9192af3c91a7ec57647e3934057340b4cf408d5a56592f8274eec53f0",
        ),
    ];
    for (kind, digest) in vectors {
        let got: String = rt
            .block_on(rt.eval(
                format!("return require('crypto').hash('{kind}', 'abc')"),
                "=test",
            ))
            .unwrap();
        assert_eq!(got, digest, "{kind}");
    }
}

#[test]
fn base64_round_trips_text_in_both_alphabets() {
    let rt = Runtime::new(Profile::Trusted).unwrap();
    let ok: bool = rt
        .block_on(rt.eval(
            r#"
            local b64 = require("crypto").base64
            local text = "hello, world?>"
            return b64.decode(b64.encode(text)) == text
               and b64.decode_urlsafe(b64.encode_urlsafe(text)) == text
               and b64.encode("hello, world") == "aGVsbG8sIHdvcmxk"
            "#,
            "=test",
        ))
        .unwrap();
    assert!(ok);
}

#[test]
fn decoding_malformed_base64_is_an_error_a_script_can_pcall() {
    let rt = Runtime::new(Profile::Trusted).unwrap();
    let failed: bool = rt
        .block_on(rt.eval(
            "local ok = pcall(require('crypto').base64.decode, '!!!') return not ok",
            "=test",
        ))
        .unwrap();
    assert!(failed);
}

// -- serde -------------------------------------------------------------------------------------

#[test]
fn json_round_trips_a_table() {
    let rt = Runtime::new(Profile::Trusted).unwrap();
    let ok: bool = rt
        .block_on(rt.eval(
            r#"
            local json = require("serde").json
            local text = json.encode({ name = "x", list = { 1, 2, 3 }, nested = { flag = true } })
            local back = json.decode(text)
            return back.name == "x" and #back.list == 3 and back.list[3] == 3 and back.nested.flag
            "#,
            "=test",
        ))
        .unwrap();
    assert!(ok);
}

#[test]
fn decoding_malformed_json_is_an_error_a_script_can_pcall() {
    let rt = Runtime::new(Profile::Trusted).unwrap();
    let failed: bool = rt
        .block_on(rt.eval(
            "local ok = pcall(require('serde').json.decode, '{nope') return not ok",
            "=test",
        ))
        .unwrap();
    assert!(failed);
}

// -- datetime ----------------------------------------------------------------------------------

#[test]
fn datetime_parses_and_formats_an_instant() {
    let rt = Runtime::new(Profile::Trusted).unwrap();
    let got: String = rt
        .block_on(rt.eval(
            r#"return tostring(require("datetime").new("2024-01-02T03:04:05Z"))"#,
            "=test",
        ))
        .unwrap();
    assert_eq!(got, "2024-01-02T03:04:05+00:00");
}

#[test]
fn datetime_builds_from_civil_fields() {
    let rt = Runtime::new(Profile::Trusted).unwrap();
    let got: String = rt
        .block_on(rt.eval(
            r#"return tostring(require("datetime").new(2024, 1, 2, 3, 4, 5))"#,
            "=test",
        ))
        .unwrap();
    assert_eq!(got, "2024-01-02T03:04:05+00:00");
}

// -- Reaching the rest -------------------------------------------------------------------------

#[test]
fn uuid_and_env_get_work() {
    let rt = Runtime::new(Profile::Trusted).unwrap();
    let ok: bool = rt
        .block_on(rt.eval(
            r#"
            local utils = require("utils")
            return type(utils.uuid()) == "string" and #utils.uuid() == 36
               and utils.env.get("AVARICE_RT_SURELY_UNSET_VARIABLE") == nil
               and type(utils.env.get("PATH")) == "string"
            "#,
            "=test",
        ))
        .unwrap();
    assert!(ok);
}
