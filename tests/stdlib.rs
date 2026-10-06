//! The stdlib modules, reached through `require`, and the selection of which a runtime has.
//!
//! Every test drives `require` and calls what comes back. None calls
//! `stdlib::loader` directly, because an embedder cannot: a module that works when
//! called directly but is not reachable through `require` is broken in the only way that matters.

mod common;

#[cfg(feature = "stdlib-crypto")]
use avarice::FsStore;
use avarice::{Profile, Runtime, StdModules};
#[cfg(any(feature = "stdlib-crypto", feature = "stdlib-fs", feature = "stdlib-http"))]
use common::TempDir;
use common::{compiled_in, listed, requirable};

const NAMES: [&str; 10] = [
    "http",
    "fs",
    "crypto",
    "serde",
    "datetime",
    "utils",
    "stores",
    "validation",
    "dirs",
    "process",
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
               and utils.env.get("AVARICE_SURELY_UNSET_VARIABLE") == nil
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

#[cfg(feature = "stdlib-http")]
/// Serves `response` once, on a port of its own, and returns the URL to ask for.
fn serve_once(response: &'static [u8]) -> String {
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
        stream.write_all(response).unwrap();
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
        b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nSet-Cookie: session=SECRET\r\n\
          Connection: close\r\n\r\nhello",
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

// -- dirs ----------------------------------------------------------------------------------
//
// These tests never rely on `$XDG_CONFIG_HOME` or similar being set or unset one way rather than
// another: the test binary runs tests in parallel, in one process, and mutating the environment
// mid-run is exactly the unsoundness `utils.rs`'s removed `setenv` was cut for (see its header).
// What is asserted instead is what holds regardless of the host's environment: that a path names
// the application asked for, that different directory kinds and different applications resolve to
// different places, and that resolving one never creates it.

#[cfg(feature = "stdlib-dirs")]
#[test]
fn config_data_and_cache_end_with_the_app_name_and_differ_from_each_other() {
    let rt = Runtime::new(Profile::Trusted).unwrap();
    let (config, data, cache): (String, String, String) = rt
        .block_on(rt.eval(
            r#"
            local app = require("dirs").app("avarice-test-app", "example.com", "Acme")
            return app:config(), app:data(), app:cache()
            "#,
            "=test",
        ))
        .unwrap();
    for path in [&config, &data, &cache] {
        assert!(
            path.ends_with("avarice-test-app"),
            "{path} does not end with the app name"
        );
    }
    assert_ne!(config, data);
    assert_ne!(config, cache);
    assert_ne!(data, cache);
}

#[cfg(feature = "stdlib-dirs")]
#[test]
fn two_different_apps_resolve_to_different_config_dirs() {
    let rt = Runtime::new(Profile::Trusted).unwrap();
    let (a, b): (String, String) = rt
        .block_on(rt.eval(
            r#"
            local dirs = require("dirs")
            return dirs.app("avarice-test-app-a", "example.com", "Acme"):config(),
                   dirs.app("avarice-test-app-b", "example.com", "Acme"):config()
            "#,
            "=test",
        ))
        .unwrap();
    assert_ne!(a, b);
}

#[cfg(feature = "stdlib-dirs")]
#[test]
fn state_and_runtime_may_be_nil_but_are_strings_when_present() {
    // Neither is guaranteed by every strategy (ADR 0014), so this only checks the type, not
    // presence.
    let rt = Runtime::new(Profile::Trusted).unwrap();
    let (state_ok, runtime_ok): (bool, bool) = rt
        .block_on(rt.eval(
            r#"
            local app = require("dirs").app("avarice-test-app", "example.com", "Acme")
            local state = app:state()
            local runtime = app:runtime()
            return state == nil or type(state) == "string",
                   runtime == nil or type(runtime) == "string"
            "#,
            "=test",
        ))
        .unwrap();
    assert!(state_ok, "state() was neither nil nor a string");
    assert!(runtime_ok, "runtime() was neither nil nor a string");
}

#[cfg(feature = "stdlib-dirs")]
#[test]
fn app_raises_immediately_when_author_or_top_level_domain_is_missing() {
    // `dirs.app` checks eagerly, not only when a directory kind is asked for: a script that never
    // calls `:config()` and friends should still be told its call was wrong.
    let rt = Runtime::new(Profile::Trusted).unwrap();
    let message: String = rt
        .block_on(rt.eval(
            r#"
            local ok, err = pcall(require("dirs").app, "avarice-test-app")
            assert(not ok)
            return tostring(err)
            "#,
            "=test",
        ))
        .unwrap();
    assert!(message.contains("author"), "{message}");
}

#[cfg(feature = "stdlib-dirs")]
#[test]
fn app_raises_when_a_field_is_not_a_string() {
    let rt = Runtime::new(Profile::Trusted).unwrap();
    let message: String = rt
        .block_on(rt.eval(
            r#"
            local ok, err = pcall(require("dirs").app, "avarice-test-app", 42, "Acme")
            assert(not ok)
            return tostring(err)
            "#,
            "=test",
        ))
        .unwrap();
    assert!(message.contains("author"), "{message}");
}

#[cfg(all(feature = "stdlib-dirs", feature = "stdlib-fs"))]
#[test]
fn resolving_a_directory_never_creates_it() {
    let rt = Runtime::new(Profile::Trusted).unwrap();
    let (config, existed_before): (String, bool) = rt
        .block_on(rt.eval(
            r#"
            local fs = require("fs")
            local config = require("dirs")
                .app("avarice-test-app-should-not-exist", "example.com", "Acme")
                :config()
            return config, fs.exists(config)
            "#,
            "=test",
        ))
        .unwrap();
    assert!(
        !existed_before,
        "{config} already existed; pick a name this test does not collide with"
    );
}

// -- Exact bytes (ADR 0015) ---------------------------------------------------------------------
//
// Bytes cross between Lua and the stdlib as Lua strings holding exactly those bytes. Each test
// sends every byte value, or ones that are not UTF-8, through and checks none is lost or replaced.

#[cfg(any(feature = "stdlib-fs", feature = "stdlib-crypto", feature = "stdlib-http"))]
/// A Lua expression for a string of every byte value once, in order.
const ALL_BYTES: &str = "(function() local t = {} for i = 0, 255 do t[#t + 1] = string.char(i) end \
                         return table.concat(t) end)()";

#[cfg(any(feature = "stdlib-fs", feature = "stdlib-crypto", feature = "stdlib-http"))]
/// Whether `source`, run in a trusted runtime with `ALL` bound to `ALL_BYTES`, returns true.
fn holds(source: &str) -> bool {
    let rt = Runtime::new(Profile::Trusted).unwrap();
    rt.block_on(rt.eval(format!("local ALL = {ALL_BYTES}\n{source}"), "=test"))
        .unwrap()
}

#[cfg(feature = "stdlib-fs")]
#[test]
fn write_file_writes_a_strings_exact_bytes() {
    let dir = TempDir::new();
    let path = dir.path().join("all.bin");
    assert!(holds(&format!(
        "require('fs').write_file({path:?}, ALL) return true"
    )));
    assert_eq!(std::fs::read(&path).unwrap(), (0..=255u8).collect::<Vec<_>>());
}

#[cfg(feature = "stdlib-fs")]
#[test]
fn write_file_still_accepts_a_table_of_byte_values() {
    let dir = TempDir::new();
    let path = dir.path().join("table.bin");
    assert!(holds(&format!(
        "require('fs').write_file({path:?}, {{ 255, 0, 1 }}) return true"
    )));
    assert_eq!(std::fs::read(&path).unwrap(), [255, 0, 1]);
}

#[cfg(feature = "stdlib-fs")]
#[test]
fn read_file_bytes_returns_the_files_exact_bytes_as_a_string() {
    let dir = TempDir::new();
    let path = dir.path().join("all.bin");
    std::fs::write(&path, (0..=255u8).collect::<Vec<_>>()).unwrap();
    assert!(holds(&format!(
        "local got = require('fs').read_file_bytes({path:?})
         return type(got) == 'string' and got == ALL"
    )));
}

#[cfg(feature = "stdlib-fs")]
#[test]
fn a_buffer_gives_its_exact_bytes_as_a_string_and_its_length_through_len() {
    let dir = TempDir::new();
    let path = dir.path().join("all.bin");
    std::fs::write(&path, (0..=255u8).collect::<Vec<_>>()).unwrap();
    assert!(holds(&format!(
        "local fs = require('fs')
         local buffer = fs.new_buffer(512)
         fs.open({path:?}):read_buf(buffer)
         return buffer:bytes() == ALL and #buffer == 256"
    )));
}

#[cfg(feature = "stdlib-fs")]
#[test]
fn a_buffer_has_no_text_method() {
    // `bytes()` already gives a string; a `text()` beside it could only differ by replacing what
    // is not UTF-8.
    assert!(holds(
        "local buffer = require('fs').new_buffer(8)
         return not pcall(function() return buffer:text() end)"
    ));
}

#[cfg(feature = "stdlib-fs")]
#[test]
fn a_buffers_json_fails_on_bytes_that_are_not_utf8_and_keeps_null_as_nil() {
    let dir = TempDir::new();
    let bad = dir.path().join("bad.json");
    std::fs::write(&bad, b"{\"a\": \"\xff\"}").unwrap();
    let good = dir.path().join("good.json");
    std::fs::write(&good, b"{\"a\": 1, \"b\": null}").unwrap();
    assert!(holds(&format!(
        "local fs = require('fs')
         local function buffer_of(path)
           local buffer = fs.new_buffer(64)
           fs.open(path):read_buf(buffer)
           return buffer
         end
         local bad = buffer_of({bad:?})
         local good = buffer_of({good:?}):json()
         return not pcall(bad.json, bad) and good.a == 1 and good.b == nil"
    )));
}

#[cfg(feature = "stdlib-crypto")]
#[test]
fn hashing_accepts_bytes_that_are_not_utf8() {
    // Digests of the three bytes FF 00 01, from `sha256sum` and Python's `hashlib.sha3_256`.
    assert!(holds(
        "local crypto = require('crypto')
         return crypto.hash('sha2_256', '\\255\\0\\1')
                  == '942e1e2a66a427b6551732f758bc314f22b9cdec9365a3425c9184de299392b5'
            and crypto.hash('sha3_256', '\\255\\0\\1')
                  == '1d87dc04bfe2f3eb321b0b0e03d35bbc84bd7191847fa54300a184449c23bedb'"
    ));
}

#[cfg(feature = "stdlib-crypto")]
#[test]
fn base64_round_trips_every_byte_value_in_both_alphabets() {
    assert!(holds(
        "local b64 = require('crypto').base64
         return b64.decode(b64.encode(ALL)) == ALL
            and b64.decode_urlsafe(b64.encode_urlsafe(ALL)) == ALL
            and b64.encode('\\255\\254\\253') == '//79'
            and b64.encode_urlsafe('\\255\\254\\253') == '__79'
            and b64.decode('/wAB') == '\\255\\0\\1'"
    ));
}

#[cfg(feature = "stdlib-http")]
/// Answers one request, on a port of its own, with a body that echoes the request's body, and
/// hands over the whole request as received. Returns the URL to ask for.
fn serve_echo_once() -> (String, std::sync::mpsc::Receiver<Vec<u8>>) {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/echo", listener.local_addr().unwrap());
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut seen = Vec::new();
        let mut chunk = [0; 512];
        let head_end = loop {
            if let Some(i) = seen.windows(4).position(|w| w == b"\r\n\r\n") {
                break i + 4;
            }
            let n = stream.read(&mut chunk).unwrap();
            assert_ne!(n, 0, "the connection closed before the request's head ended");
            seen.extend_from_slice(&chunk[..n]);
        };
        let head = String::from_utf8_lossy(&seen[..head_end]).to_ascii_lowercase();
        let length: usize = head
            .lines()
            .find_map(|line| line.strip_prefix("content-length:"))
            .map_or(0, |n| n.trim().parse().unwrap());
        while seen.len() < head_end + length {
            let n = stream.read(&mut chunk).unwrap();
            assert_ne!(n, 0, "the connection closed before the request's body ended");
            seen.extend_from_slice(&chunk[..n]);
        }
        let body = &seen[head_end..head_end + length];
        let mut response =
            format!("HTTP/1.1 200 OK\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n")
                .into_bytes();
        response.extend_from_slice(body);
        stream.write_all(&response).unwrap();
        sender.send(seen).unwrap();
    });
    (url, receiver)
}

#[cfg(feature = "stdlib-http")]
#[test]
fn a_request_body_given_as_a_string_is_sent_exactly_and_the_response_body_read_exactly() {
    let (url, request) = serve_echo_once();
    assert!(holds(&format!(
        "local body = require('http').request('{url}'):set_method('POST'):set_body(ALL)
           :execute():body()
         return body:bytes() == ALL and #body == 256"
    )));
    let request = request.recv().unwrap();
    assert!(request.ends_with(&(0..=255u8).collect::<Vec<_>>()));
    // The default content type for a string body is unchanged.
    let head = String::from_utf8_lossy(&request).to_ascii_lowercase();
    assert!(head.contains("content-type: text/plain"), "{head}");
}

#[cfg(feature = "stdlib-http")]
#[test]
fn a_request_body_given_as_a_table_of_byte_values_is_still_sent() {
    let (url, request) = serve_echo_once();
    assert!(holds(&format!(
        "require('http').request('{url}'):set_method('POST'):set_body({{ 255, 0, 1 }}):execute()
         return true"
    )));
    assert!(request.recv().unwrap().ends_with(&[255, 0, 1]));
}

#[cfg(feature = "stdlib-http")]
/// A response whose `X-Bytes` header value is `caf` and then E9, which is Latin-1 for `é` and not
/// UTF-8. HTTP allows such bytes in a header value.
const LATIN1_HEADER_RESPONSE: &[u8] =
    b"HTTP/1.1 200 OK\r\nX-Bytes: caf\xe9\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

#[cfg(feature = "stdlib-http")]
#[test]
fn a_response_header_value_that_is_not_utf8_reaches_lua_exactly() {
    let url = serve_once(LATIN1_HEADER_RESPONSE);
    assert!(holds(&format!(
        "return require('http').request('{url}'):execute():headers()['x-bytes'] == 'caf\\233'"
    )));
}

#[cfg(feature = "stdlib-http")]
#[test]
fn a_streamed_response_header_value_that_is_not_utf8_reaches_lua_exactly() {
    let url = serve_once(LATIN1_HEADER_RESPONSE);
    let rt = Runtime::new(Profile::Trusted).unwrap();
    rt.block_on(rt.exec(
        format!(
            "require('http').request('{url}'):execute_streaming(function(response)
               got = got or response:headers()['x-bytes']
             end)"
        ),
        "=test",
    ))
    .unwrap();
    // The callback runs on a task of its own, after `execute_streaming` returns.
    rt.block_on(rt.wait_for_tasks()).unwrap();
    let exact: bool = rt
        .block_on(rt.eval("return got == 'caf\\233'", "=test"))
        .unwrap();
    assert!(exact);
}

#[cfg(feature = "stdlib-http")]
#[test]
fn a_url_that_is_not_utf8_is_an_error_whether_given_as_a_string_or_in_a_table() {
    // A URL is text the script writes; neither form may quietly replace what is not UTF-8.
    assert!(holds(
        "local http = require('http')
         local url = 'http://example.invalid/caf\\233'
         return not pcall(http.request, url) and not pcall(http.request, { url = url })"
    ));
}

#[cfg(feature = "stdlib-http")]
/// Sends a request with `set_file(<form>)`, `form` being Lua source, to the echo server, and hands
/// back the request the server received, or the error the send raised.
fn upload(form: &str) -> Result<Vec<u8>, String> {
    let (url, request) = serve_echo_once();
    let rt = Runtime::new(Profile::Trusted).unwrap();
    let error: Option<String> = rt
        .block_on(rt.eval(
            format!(
                "local ok, err = pcall(function()
                   require('http').request('{url}'):set_method('POST'):set_file({form}):execute()
                 end)
                 if not ok then return tostring(err) end"
            ),
            "=test",
        ))
        .unwrap();
    match error {
        Some(error) => Err(error),
        None => Ok(request.recv().unwrap()),
    }
}

#[cfg(feature = "stdlib-http")]
/// Whether the request the echo server received carries `contents`.
fn carries(request: &[u8], contents: &[u8]) -> bool {
    request.windows(contents.len()).any(|w| w == contents)
}

#[cfg(feature = "stdlib-http")]
#[test]
fn set_file_uploads_each_entry_of_a_table_or_a_list() {
    let dir = TempDir::new();
    let one = dir.write("one.txt", "FIRST-FILE");
    let two = dir.write("two.txt", "SECOND-FILE");
    for (form, expected) in [
        (format!("{one:?}"), &["FIRST-FILE"][..]),
        (format!("{{ name = 'f', path = {one:?} }}"), &["FIRST-FILE"][..]),
        (
            format!("{{ {{ name = 'f', path = {one:?} }}, {{ name = 'g', path = {two:?} }} }}"),
            &["FIRST-FILE", "SECOND-FILE"][..],
        ),
    ] {
        let request = upload(&form).unwrap();
        for contents in expected {
            assert!(carries(&request, contents.as_bytes()), "{form}: no {contents}");
        }
    }
}

#[cfg(feature = "stdlib-http")]
#[test]
fn a_file_to_upload_that_cannot_be_read_is_an_error_when_the_request_is_sent() {
    // ADR 0017: Astra sent the request without such an entry, and it succeeded.
    let dir = TempDir::new();
    let good = dir.write("good.txt", "GOOD-FILE");
    for (form, message) in [
        ("{ name = 'f' }".to_string(), "needs a string `path`"),
        (format!("{{ path = {good:?} }}"), "needs a string `name`"),
        (
            format!("{{ {{ name = 'f', path = {good:?} }}, {{ name = 'g' }} }}"),
            "needs a string `path`",
        ),
        (
            format!("{{ {{ name = 'f', path = {good:?} }}, 'not an entry' }}"),
            "something other than a table",
        ),
        // A `name` makes a table one entry, so a list must not have one.
        (
            format!("{{ name = 'x', {{ name = 'f', path = {good:?} }} }}"),
            "needs a string `path`",
        ),
        ("5".to_string(), "a path, a `{ name, path }` table, or a list of them"),
    ] {
        match upload(&form) {
            Ok(_) => panic!("{form} was sent without the file"),
            Err(error) => assert!(error.contains(message), "{form}: {error}"),
        }
    }
}

#[cfg(all(unix, feature = "stdlib-http"))]
#[test]
fn a_file_in_a_directory_whose_name_is_not_utf8_is_found_by_its_exact_bytes() {
    use std::os::unix::ffi::OsStrExt;
    let dir = TempDir::new();
    // `caf` and then E9: Latin-1 for `café`, which Linux allows in a name.
    let latin1 = dir.path().join(std::ffi::OsStr::from_bytes(b"caf\xe9"));
    std::fs::create_dir(&latin1).unwrap();
    std::fs::write(latin1.join("in.txt"), "IN-A-LATIN1-DIRECTORY").unwrap();
    let path = format!("{:?} .. '/caf\\233/in.txt'", dir.path());
    for form in [path.clone(), format!("{{ name = 'f', path = {path} }}")] {
        let request = upload(&form).unwrap();
        assert!(carries(&request, b"IN-A-LATIN1-DIRECTORY"), "{form}");
    }
}

#[cfg(all(unix, feature = "stdlib-http"))]
#[test]
fn a_file_whose_own_name_is_not_utf8_is_an_error_since_its_name_is_sent_as_text() {
    // The name goes into the request as text, and reqwest can send only UTF-8 there, so it would
    // otherwise be sent with U+FFFD in place of what is not UTF-8.
    use std::os::unix::ffi::OsStrExt;
    let dir = TempDir::new();
    std::fs::write(
        dir.path().join(std::ffi::OsStr::from_bytes(b"caf\xe9.txt")),
        "LATIN1-NAMED-FILE",
    )
    .unwrap();
    let path = format!("{:?} .. '/caf\\233.txt'", dir.path());
    for form in [path.clone(), format!("{{ name = 'f', path = {path} }}")] {
        match upload(&form) {
            Ok(_) => panic!("{form} was sent"),
            Err(error) => assert!(error.contains("is not UTF-8"), "{form}: {error}"),
        }
    }
}
