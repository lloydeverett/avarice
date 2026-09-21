//! The stdlib modules, reached through `require`, and the selection of which a runtime has.
//!
//! Every test drives `require` and calls what comes back. None calls
//! `stdlib::loader` directly, because an embedder cannot: a module that works when
//! called directly but is not reachable through `require` is broken in the only way that matters.

mod common;

#[cfg(feature = "stdlib-crypto")]
use avarice_rt::FsStore;
use avarice_rt::{Profile, Runtime, StdModules};
#[cfg(any(feature = "stdlib-crypto", feature = "stdlib-fs"))]
use common::TempDir;
use common::{compiled_in, listed, requirable};

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

/// The names of the pure modules that are compiled in: what a sandbox registers (ADR 0007).
fn pure_names() -> Vec<&'static str> {
    StdModules::PURE.modules().map(|m| m.name()).collect()
}

/// What a sandbox with `extra` added to it registers: the pure modules and those, in `NAMES` order.
fn sandbox_plus(extra: &[&str]) -> Vec<&'static str> {
    let pure = pure_names();
    NAMES
        .into_iter()
        .filter(|name| pure.contains(name) || extra.contains(name))
        .collect()
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
fn trusted_mode_registers_every_stdlib_module_that_is_compiled_in() {
    let rt = Runtime::new(Profile::Trusted).unwrap();
    assert_eq!(reachable(&rt), compiled_in());
    for name in compiled_in() {
        assert!(rt.has_module(name), "{name}");
    }
}

#[test]
fn sandbox_mode_registers_the_pure_modules_and_says_module_not_found_for_the_rest() {
    let rt = Runtime::new(Profile::Sandbox).unwrap();
    let pure = pure_names();
    assert_eq!(reachable(&rt), pure);
    assert_eq!(listed(&rt), pure);
    for name in NAMES.into_iter().filter(|name| !pure.contains(name)) {
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
    let expected: Vec<_> = compiled_in()
        .into_iter()
        .filter(|name| *name != "http")
        .collect();
    assert_eq!(reachable(&rt), expected);
}

#[cfg(feature = "stdlib-validation")]
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
    assert_eq!(reachable(&with), sandbox_plus(&["validation"]));
}

#[cfg(feature = "stdlib-fs")]
#[test]
fn sandbox_plus_one_module_has_that_one_and_no_other() {
    let rt = Runtime::builder(Profile::Sandbox)
        .with_std_modules(StdModules::FS)
        .build()
        .unwrap();
    assert!(rt.has_module("fs"));
    assert_eq!(reachable(&rt), sandbox_plus(&["fs"]));
}

#[cfg(all(feature = "stdlib-crypto", feature = "stdlib-stores"))]
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
    assert_eq!(reachable(&rt), compiled_in());
}

#[cfg(feature = "stdlib-crypto")]
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

#[cfg(feature = "stdlib-stores")]
#[test]
fn a_stdlib_module_is_built_once() {
    let rt = Runtime::new(Profile::Trusted).unwrap();
    let same: bool = rt
        .block_on(rt.eval("return require('stores') == require('stores')", "=test"))
        .unwrap();
    assert!(same);
}

#[cfg(feature = "stdlib-crypto")]
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

#[cfg(feature = "stdlib-crypto")]
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

#[cfg(feature = "stdlib-stores")]
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

#[cfg(feature = "stdlib-stores")]
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

#[cfg(feature = "stdlib-stores")]
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

#[cfg(feature = "stdlib-crypto")]
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

#[cfg(feature = "stdlib-crypto")]
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

#[cfg(feature = "stdlib-crypto")]
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

#[cfg(feature = "stdlib-serde")]
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

#[cfg(feature = "stdlib-serde")]
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

#[cfg(feature = "stdlib-datetime")]
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

#[cfg(feature = "stdlib-datetime")]
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

#[cfg(feature = "stdlib-utils")]
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

// -- `stdlib()` --------------------------------------------------------------------------------

#[test]
fn stdlib_lists_the_names_require_takes() {
    let rt = Runtime::new(Profile::Trusted).unwrap();
    assert_eq!(listed(&rt), compiled_in());
}

#[test]
fn stdlib_lists_what_this_runtime_has_and_nothing_else() {
    assert_eq!(
        listed(&Runtime::new(Profile::Sandbox).unwrap()),
        pure_names()
    );

    let rt = Runtime::builder(Profile::Trusted)
        .without_std_modules(StdModules::HTTP)
        .build()
        .unwrap();
    let expected: Vec<_> = compiled_in()
        .into_iter()
        .filter(|name| *name != "http")
        .collect();
    assert_eq!(listed(&rt), expected);
}

#[cfg(all(feature = "stdlib-fs", feature = "stdlib-validation"))]
#[test]
fn stdlib_lists_the_modules_added_to_a_sandbox() {
    let rt = Runtime::builder(Profile::Sandbox)
        .with_std_modules(StdModules::FS | StdModules::VALIDATION)
        .build()
        .unwrap();
    assert_eq!(listed(&rt), sandbox_plus(&["fs", "validation"]));
}

#[test]
fn every_name_stdlib_lists_can_be_required() {
    let rt = Runtime::new(Profile::Trusted).unwrap();
    for name in listed(&rt) {
        assert!(requirable(&rt, &name), "{name}");
    }
}

#[test]
fn stdlib_does_not_build_any_module() {
    let rt = Runtime::new(Profile::Trusted).unwrap();
    let untouched: bool = rt
        .block_on(rt.eval(
            "stdlib() return astra_internal__hash == nil and package == nil",
            "=test",
        ))
        .unwrap();
    assert!(untouched);
}

#[test]
fn stdlib_hands_out_a_fresh_list_each_call() {
    let rt = Runtime::new(Profile::Trusted).unwrap();
    let names = compiled_in();
    let first = names
        .first()
        .map_or("nil".to_string(), |name| format!("{name:?}"));
    let independent: bool = rt
        .block_on(rt.eval(
            format!(
                "local a = stdlib() a[1] = 'changed' table.remove(a) \
                 return stdlib()[1] == {first} and #stdlib() == {}",
                names.len()
            ),
            "=test",
        ))
        .unwrap();
    assert!(independent);
}

// -- Printing what the modules hand back --------------------------------------------------------
//
// A userdata's contents live in Rust and Lua cannot enumerate them, so what `print` shows for one
// is what its `__tostring` says. These check what `fs`'s do, through `print`.

#[cfg(feature = "stdlib-fs")]
/// What a trusted runtime prints for `source`, which is given `fs` and `dir`, as a Lua string.
fn printed_in_fs(dir: &TempDir, source: &str) -> String {
    let buffer = common::Buffer::new();
    let rt = Runtime::builder(Profile::Trusted)
        .write_sink(buffer.clone())
        .build()
        .unwrap();
    let prelude = format!("local fs, dir = require('fs'), {:?}\n", dir.path());
    rt.block_on(rt.exec(format!("{prelude}{source}"), "=test"))
        .unwrap();
    common::plain(&buffer.contents())
}

#[cfg(feature = "stdlib-fs")]
#[test]
fn a_dir_entry_prints_its_type_name_and_path() {
    let dir = TempDir::new();
    let file = dir.write("a.txt", "");
    let printed = printed_in_fs(&dir, "print(fs.read_dir(dir)[1])");
    assert_eq!(printed, format!("AstraDirEntry({})\n", file.display()));
}

#[cfg(feature = "stdlib-fs")]
#[test]
fn tostring_of_a_dir_entry_is_what_print_shows() {
    let dir = TempDir::new();
    let file = dir.write("a.txt", "");
    let text = printed_in_fs(&dir, "print(tostring(fs.read_dir(dir)[1]))");
    assert_eq!(text, format!("AstraDirEntry({})\n", file.display()));
}

#[cfg(feature = "stdlib-fs")]
#[test]
fn a_directory_listing_shows_each_entry_rather_than_its_address() {
    let dir = TempDir::new();
    let a = dir.write("a.txt", "");
    let b = dir.write("sub/b.txt", "");
    let printed = printed_in_fs(&dir, "print(fs.read_dir(dir))");
    assert!(!printed.contains("0x"), "{printed}");
    // `read_dir` gives no order, so each entry is looked for rather than the listing compared.
    assert!(
        printed.contains(&format!("AstraDirEntry({}),", a.display())),
        "{printed}"
    );
    assert!(
        printed.contains(&format!(
            "AstraDirEntry({}),",
            b.parent().unwrap().display()
        )),
        "{printed}"
    );
}

#[cfg(feature = "stdlib-fs")]
#[test]
fn an_entry_type_prints_what_kind_of_entry_it_is() {
    let dir = TempDir::new();
    dir.write("a.txt", "");
    dir.write("sub/b.txt", "");
    let printed = printed_in_fs(
        &dir,
        "print(fs.get_metadata(dir .. '/a.txt'):type())
         print(fs.get_metadata(dir .. '/sub'):type())",
    );
    assert_eq!(printed, "AstraEntryType(file)\nAstraEntryType(dir)\n");
}

#[cfg(feature = "stdlib-fs")]
#[test]
#[cfg(unix)]
fn a_symlink_prints_as_a_symlink() {
    let dir = TempDir::new();
    let target = dir.write("target.txt", "");
    std::os::unix::fs::symlink(&target, dir.path().join("link")).unwrap();
    let printed = printed_in_fs(
        &dir,
        "for _, entry in ipairs(fs.read_dir(dir)) do
           if entry:file_name() == 'link' then print(entry:type()) end
         end",
    );
    assert_eq!(printed, "AstraEntryType(symlink)\n");
}

#[cfg(feature = "stdlib-fs")]
#[test]
#[cfg(unix)]
fn a_dir_entry_whose_name_is_not_utf8_still_prints() {
    use std::os::unix::ffi::OsStrExt;
    let dir = TempDir::new();
    let name = std::ffi::OsStr::from_bytes(b"caf\xe9");
    std::fs::write(dir.path().join(name), "").unwrap();
    // `entry:path()` raises for this name. Printing must not: a directory listing is what one
    // reaches for to find out why something is odd.
    let printed = printed_in_fs(&dir, "print(fs.read_dir(dir)[1])");
    assert!(printed.starts_with("AstraDirEntry("), "{printed}");
    assert!(printed.contains("caf\u{fffd}"), "{printed}");
}

#[cfg(feature = "stdlib-fs")]
#[test]
fn a_file_prints_its_path() {
    let dir = TempDir::new();
    let file = dir.write("a.txt", "hello");
    let printed = printed_in_fs(&dir, "print(fs.open(dir .. '/a.txt'))");
    assert_eq!(printed, format!("AstraFile({})\n", file.display()));
}

#[cfg(feature = "stdlib-fs")]
#[test]
fn metadata_prints_its_type_and_length() {
    let dir = TempDir::new();
    dir.write("a.txt", "hello");
    dir.write("sub/b.txt", "");
    let printed = printed_in_fs(
        &dir,
        "print(fs.get_metadata(dir .. '/a.txt'))
         print(fs.get_metadata(dir .. '/sub'))",
    );
    let mut lines = printed.lines();
    assert_eq!(lines.next(), Some("AstraMetadata(file, len 5)"));
    // A directory's length is the filesystem's to say.
    let second = lines.next().unwrap();
    assert!(second.starts_with("AstraMetadata(dir, len "), "{second}");
}

#[cfg(feature = "stdlib-fs")]
#[test]
fn file_permissions_print_whether_they_are_readonly() {
    let dir = TempDir::new();
    let file = dir.write("a.txt", "");
    let printed = printed_in_fs(
        &dir,
        "local p = fs.get_metadata(dir .. '/a.txt'):file_permissions()
         print(p)
         p:set_readonly(true)
         print(p)",
    );
    assert_eq!(
        printed,
        "AstraFilePermissions(read-write)\nAstraFilePermissions(readonly)\n"
    );
    // `set_readonly` changes the value in Lua, and never the file.
    assert!(!std::fs::metadata(file).unwrap().permissions().readonly());
}

#[cfg(feature = "stdlib-fs")]
#[test]
fn a_buffer_prints_its_length_and_never_its_contents() {
    let dir = TempDir::new();
    dir.write("a.txt", "hello world");
    let printed = printed_in_fs(
        &dir,
        "local buffer = fs.new_buffer(64)
         print(buffer)
         local file = fs.open(dir .. '/a.txt')
         file:read_buf(buffer)
         print(buffer)",
    );
    assert_eq!(printed, "AstraBufferMut(len 0)\nAstraBufferMut(len 11)\n");
}

#[cfg(all(feature = "stdlib-fs", feature = "stdlib-validation"))]
#[test]
fn a_regex_prints_its_pattern() {
    let dir = TempDir::new();
    let printed = printed_in_fs(&dir, "print(require('validation').regex('(\\\\d+)-x'))");
    assert_eq!(printed, "AstraRegex(/(\\d+)-x/)\n");
}

#[cfg(all(feature = "stdlib-fs", feature = "stdlib-http"))]
/// Serves `response` once, on a port of its own, and returns the URL to ask for.
fn serve_once(response: &'static str) -> String {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/where", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut seen = Vec::new();
        let mut chunk = [0; 512];
        while !seen.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = stream.read(&mut chunk).unwrap();
            if n == 0 {
                break;
            }
            seen.extend_from_slice(&chunk[..n]);
        }
        stream.write_all(response.as_bytes()).unwrap();
    });
    url
}

#[cfg(all(feature = "stdlib-fs", feature = "stdlib-http"))]
#[test]
fn an_http_request_prints_its_method_and_url() {
    let dir = TempDir::new();
    let printed = printed_in_fs(
        &dir,
        "local http = require('http')
         print(http.request('http://example.invalid/x'))
         print(http.request({ url = 'http://example.invalid/y', method = 'POST' }))
         print(http.request('http://example.invalid/z'):set_method('PUT'))",
    );
    assert_eq!(
        printed,
        "HTTPClientRequest(GET http://example.invalid/x)\n\
         HTTPClientRequest(POST http://example.invalid/y)\n\
         HTTPClientRequest(PUT http://example.invalid/z)\n"
    );
}

#[cfg(all(feature = "stdlib-fs", feature = "stdlib-http"))]
#[test]
fn an_http_request_does_not_print_its_headers_or_body() {
    // A request is printed to find out which one it is, and a header is where a token lives.
    let dir = TempDir::new();
    let printed = printed_in_fs(
        &dir,
        "print(require('http').request({
           url = 'http://example.invalid/',
           method = 'POST',
           headers = { Authorization = 'Bearer SECRET-TOKEN' },
           body = 'SECRET-BODY',
         }))",
    );
    assert!(!printed.contains("SECRET"), "{printed}");
    assert!(printed.starts_with("HTTPClientRequest(POST "), "{printed}");
}

#[cfg(all(feature = "stdlib-fs", feature = "stdlib-http"))]
#[test]
fn an_http_response_prints_its_status_and_url_and_its_body_prints_its_length() {
    let url = serve_once(
        "HTTP/1.1 200 OK\r\nContent-Length: 5\r\nSet-Cookie: session=SECRET\r\nConnection: close\r\n\r\nhello",
    );
    let dir = TempDir::new();
    let printed = printed_in_fs(
        &dir,
        &format!(
            "local response = require('http').request('{url}'):execute()
             print(response)
             print(response:body())"
        ),
    );
    assert_eq!(
        printed,
        format!("HTTPClientResponse(200 {url})\nAstraBuffer(len 5)\n")
    );
    assert!(!printed.contains("SECRET"), "{printed}");
}

#[cfg(all(feature = "stdlib-fs", feature = "stdlib-utils"))]
#[test]
fn a_task_handle_prints_where_the_task_has_got_to() {
    let dir = TempDir::new();
    let printed = printed_in_fs(
        &dir,
        "local utils = require('utils')
         local quick = utils.spawn_task(function() end)
         print(quick)                      -- spawned, not yet run
         utils.spawn_timeout(function() end, 30):await()
         print(quick)                      -- ran while that waited
         quick:await()
         print(quick)
         local long = utils.spawn_timeout(function() end, 1000)
         long:abort()
         print(long)",
    );
    assert_eq!(
        printed,
        "TaskHandler(running)\nTaskHandler(finished)\n\
         TaskHandler(awaited or aborted)\nTaskHandler(awaited or aborted)\n"
    );
}

#[cfg(all(feature = "stdlib-fs", feature = "stdlib-utils"))]
#[test]
fn printing_a_task_handle_that_is_being_awaited_does_not_raise() {
    // `await` holds the handle for as long as it waits, so anything else that reads it meanwhile
    // finds it taken. Printing is how one finds out what is going on, and must not fail there.
    let dir = TempDir::new();
    let printed = printed_in_fs(
        &dir,
        "local utils = require('utils')
         local slow = utils.spawn_timeout(function() end, 200)
         utils.spawn_task(function() slow:await() end)
         utils.spawn_timeout(function() print(slow) end, 20):await()",
    );
    assert_eq!(printed, "TaskHandler(awaiting)\n");
}
