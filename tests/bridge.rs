//! The `bridge` module (ADR 0022), driven the way a script does: Lua through a trusted runtime.
//!
//! A Bridge table is written and read as a Lua table is, so most of these run the same Lua on a
//! Bridge table that a plain table would answer, and check the answer a plain table gives.

#![cfg(feature = "stdlib-bridge")]

mod common;

use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use avarice::mlua::{self, Value};
use avarice::{
    CancelHandle, Error, Profile, Runtime, StdModules, bridge, was_cancelled, was_out_of_memory,
};

/// Runs `body` with `bridge` bound to the module, and returns what it returns.
fn eval<R: mlua::FromLuaMulti>(body: &str) -> R {
    let rt = Runtime::new(Profile::Trusted).unwrap();
    eval_in(&rt, body)
}

fn eval_in<R: mlua::FromLuaMulti>(rt: &Runtime, body: &str) -> R {
    let source = format!("local bridge = require('bridge')\n{body}");
    rt.block_on(rt.eval(source, "=test"))
        .unwrap_or_else(|e| panic!("{body}: {e}"))
}

/// Runs `body` with `bridge` bound to the module, expecting it to raise, and returns the message.
fn raises(body: &str) -> String {
    let rt = Runtime::new(Profile::Trusted).unwrap();
    let source = format!(
        "local bridge = require('bridge')\n\
         local ok, err = pcall(function() {body} end)\n\
         assert(not ok, 'did not raise')\n\
         return tostring(err)"
    );
    rt.block_on(rt.eval::<String>(source, "=test"))
        .unwrap_or_else(|e| panic!("{body}: {e}"))
}

// -- Making one --------------------------------------------------------------------------------

#[test]
fn bridge_table_makes_a_bridge_table_that_is_empty() {
    let got: (String, i64) = eval(
        "local t = bridge.table()
         return type(t), #t",
    );
    assert_eq!(got, ("userdata".into(), 0));
}

#[test]
fn bridge_table_with_init_starts_with_its_contents() {
    let got: (i64, String, i64) = eval(
        "local t = bridge.table({ 10, 20, name = 'x' })
         return t[2], t.name, #t",
    );
    assert_eq!(got, (20, "x".into(), 2));
}

#[test]
fn bridge_table_copies_init_so_later_writes_to_init_do_not_show_through() {
    let got: (i64, Option<i64>, i64) = eval(
        "local init = { a = 1 }
         local t = bridge.table(init)
         init.a = 2
         init.b = 3
         t.a = 4
         return t.a, t.b, init.a",
    );
    assert_eq!(got, (4, None, 2));
}

#[test]
fn bridge_table_refuses_an_init_that_is_not_a_table() {
    let message = raises("bridge.table('x')");
    assert!(
        message.contains("bridge.table: expected a table for init, got string"),
        "{message}"
    );
}

#[test]
fn bridge_table_refuses_an_argument_past_init() {
    let message = raises("bridge.table({}, {})");
    assert!(
        message.contains("bridge.table: expected at most 1 argument, got 2"),
        "{message}"
    );
}

// -- Reading and writing -----------------------------------------------------------------------

#[test]
fn a_write_reads_back_by_name_and_by_key() {
    let got: (i64, i64, String, bool) = eval(
        "local t = bridge.table()
         local key = {}
         t.x = 1
         t['y'] = 2
         t[1] = 'one'
         t[key] = true
         return t.x, t.y, t[1], t[key]",
    );
    assert_eq!(got, (1, 2, "one".into(), true));
}

#[test]
fn a_key_never_written_reads_as_nil() {
    let got: bool = eval("local t = bridge.table() return t.missing == nil and t[nil] == nil");
    assert!(got);
}

#[test]
fn writing_nil_removes_a_key() {
    let got: (bool, i64) = eval(
        "local t = bridge.table({ a = 1, b = 2 })
         t.a = nil
         local n = 0
         for _ in pairs(t) do n = n + 1 end
         return t.a == nil, n",
    );
    assert_eq!(got, (true, 1));
}

#[test]
fn a_nil_or_nan_key_raises_as_on_a_table() {
    let message = raises("bridge.table()[nil] = 1");
    assert!(message.contains("index is nil"), "{message}");
    let message = raises("bridge.table()[0/0] = 1");
    assert!(message.contains("index is NaN"), "{message}");
}

#[test]
fn keys_named_like_the_operations_are_the_scripts() {
    let got: String = eval(
        "local t = bridge.table()
         local names = { 'close', 'subscribe', 'batch', 'insert', 'remove', 'snapshot' }
         for i, name in ipairs(names) do
             assert(t[name] == nil, name .. ' is set before it was written')
             t[name] = i
         end
         local got = {}
         for i, name in ipairs(names) do got[i] = name .. '=' .. t[name] end
         return table.concat(got, ' ')",
    );
    assert_eq!(
        got,
        "close=1 subscribe=2 batch=3 insert=4 remove=5 snapshot=6"
    );
}

#[test]
fn any_lua_value_can_be_stored() {
    let got: (bool, bool, bool, bool, bool) = eval(
        "local t = bridge.table()
         local f = function() return 'called' end
         local co = coroutine.create(function() end)
         local inner = bridge.table()
         t.f, t.co, t.inner, t.flag, t.n = f, co, inner, false, 1.5
         return t.f() == 'called', t.co == co, rawequal(t.inner, inner), t.flag == false,
             t.n == 1.5",
    );
    assert_eq!(got, (true, true, true, true, true));
}

#[test]
fn a_stored_table_is_stored_by_reference() {
    let got: (bool, i64) = eval(
        "local t = bridge.table()
         local inner = { 1 }
         t.inner = inner
         inner[1] = 2
         return rawequal(t.inner, inner), t.inner[1]",
    );
    assert_eq!(got, (true, 2));
}

// -- Length and iteration ----------------------------------------------------------------------

#[test]
fn length_is_the_length_of_the_sequence() {
    let got: (i64, i64) = eval(
        "local t = bridge.table({ 'a', 'b', 'c', x = 1 })
         local before = #t
         t[4] = 'd'
         return before, #t",
    );
    assert_eq!(got, (3, 4));
}

#[test]
fn pairs_walks_every_key() {
    let got: String = eval(
        "local t = bridge.table({ 'a', 'b', x = 1, y = 2 })
         local seen = {}
         for k, v in pairs(t) do seen[#seen + 1] = tostring(k) .. '=' .. tostring(v) end
         table.sort(seen)
         return table.concat(seen, ' ')",
    );
    assert_eq!(got, "1=a 2=b x=1 y=2");
}

#[test]
fn ipairs_walks_the_sequence_in_order() {
    let got: String = eval(
        "local t = bridge.table({ 'a', 'b', 'c', x = 1 })
         local seen = {}
         for i, v in ipairs(t) do seen[#seen + 1] = i .. v end
         return table.concat(seen, ' ')",
    );
    assert_eq!(got, "1a 2b 3c");
}

#[test]
fn pairs_gives_out_nothing_that_writes_past_the_bridge_table() {
    // What `pairs` returns is the iterator, its state and the first key. None of them may be a
    // table that holds the contents, or a write to it would reach them without the Bridge table
    // knowing.
    let got: (bool, bool) = eval(
        "local t = bridge.table({ a = 1 })
         local _, state = pairs(t)
         local wrote = pcall(function() rawset(state, 'a', 99) end)
         return wrote, t.a == 1",
    );
    assert_eq!(got, (false, true));
}

#[test]
fn pairs_does_not_follow_a_script_that_replaces_next() {
    let got: i64 = eval(
        "local t = bridge.table({ 'a', 'b' })
         next = function() error('the global next was called') end
         local n = 0
         for _ in pairs(t) do n = n + 1 end
         return n",
    );
    assert_eq!(got, 2);
}

// -- The table library -------------------------------------------------------------------------

#[test]
fn table_insert_appends_and_inserts_as_on_a_table() {
    let got: String = eval(
        "local t = bridge.table({ 'b', 'd' })
         table.insert(t, 'e')
         table.insert(t, 1, 'a')
         table.insert(t, 3, 'c')
         return table.concat(t, ',')",
    );
    assert_eq!(got, "a,b,c,d,e");
}

#[test]
fn table_remove_removes_as_on_a_table() {
    let got: (String, String, String, i64) = eval(
        "local t = bridge.table({ 'a', 'b', 'c', 'd' })
         local first = table.remove(t, 1)
         local last = table.remove(t)
         return first, last, table.concat(t, ','), #t",
    );
    assert_eq!(got, ("a".into(), "d".into(), "b,c".into(), 2));
}

#[test]
fn table_concat_joins_as_on_a_table() {
    let got: (String, String) = eval(
        "local t = bridge.table({ 1, 'two', 3 })
         return table.concat(t, '-'), table.concat(t, '', 2, 3)",
    );
    assert_eq!(got, ("1-two-3".into(), "two3".into()));
}

#[test]
fn table_sort_sorts_as_on_a_table() {
    let got: (String, String) = eval(
        "local t = bridge.table({ 3, 1, 2 })
         table.sort(t)
         local up = table.concat(t, ',')
         table.sort(t, function(a, b) return a > b end)
         return up, table.concat(t, ',')",
    );
    assert_eq!(got, ("1,2,3".into(), "3,2,1".into()));
}

#[test]
fn table_unpack_unpacks_as_on_a_table() {
    let got: (String, String, String) =
        eval("return table.unpack(bridge.table({ 'a', 'b', 'c' }))");
    assert_eq!(got, ("a".into(), "b".into(), "c".into()));
}

// -- Subscribing from Lua ----------------------------------------------------------------------

/// Lua that defines `record(changes)`, which appends each change to `log` as `key:old>new`, nil
/// shown as `nil`, and `told()`, which gives the log joined with spaces.
const RECORD: &str = "
    local log = {}
    local function record(changes)
        local parts = {}
        for i, c in ipairs(changes) do
            parts[i] = tostring(c.key) .. ':' .. tostring(c.old) .. '>' .. tostring(c.new)
        end
        log[#log + 1] = '[' .. table.concat(parts, ',') .. ']'
    end
    local function told() return table.concat(log, ' ') end
";

#[test]
fn a_write_tells_a_lua_subscriber_one_change_after_it_is_in_place() {
    let got: (String, i64) = eval(&format!(
        "{RECORD}
         local t = bridge.table({{ a = 1 }})
         local seen
         bridge.table.subscribe(t, function(changes) record(changes) seen = t.a end)
         t.a = 2
         return told(), seen"
    ));
    assert_eq!(got, ("[a:1>2]".into(), 2));
}

#[test]
fn an_add_has_nil_old_and_a_removal_nil_new() {
    let got: String = eval(&format!(
        "{RECORD}
         local t = bridge.table({{ gone = true }})
         bridge.table.subscribe(t, record)
         t.added = 'x'
         t.gone = nil
         return told()"
    ));
    assert_eq!(got, "[added:nil>x] [gone:true>nil]");
}

#[test]
fn a_write_tells_every_lua_subscriber_in_the_order_they_subscribed() {
    let got: String = eval(
        "local t = bridge.table()
         local order = {}
         for i = 1, 3 do
             bridge.table.subscribe(t, function(changes)
                 order[#order + 1] = i .. changes[1].key
             end)
         end
         t.k = 1
         return table.concat(order, ' ')",
    );
    assert_eq!(got, "1k 2k 3k");
}

#[test]
fn the_table_library_writes_are_told() {
    let got: String = eval(&format!(
        "{RECORD}
         local t = bridge.table({{ 'a' }})
         bridge.table.subscribe(t, record)
         table.insert(t, 'b')
         table.remove(t)
         return told()"
    ));
    assert_eq!(got, "[2:nil>b] [2:b>nil]");
}

#[test]
fn unsubscribe_stops_a_lua_subscriber() {
    let got: (String, String) = eval(&format!(
        "{RECORD}
         local t = bridge.table()
         local handle = bridge.table.subscribe(t, record)
         t.a = 1
         handle:unsubscribe()
         t.b = 2
         handle:unsubscribe()
         local first = told()
         bridge.table.subscribe(t, record)
         t.c = 3
         return first, told()"
    ));
    assert_eq!(got, ("[a:nil>1]".into(), "[a:nil>1] [c:nil>3]".into()));
}

#[test]
fn subscribe_refuses_something_that_is_not_a_function() {
    let message = raises("bridge.table.subscribe(bridge.table(), 'x')");
    assert!(
        message.contains("bridge.table.subscribe: expected a function, got string"),
        "{message}"
    );
}

#[test]
fn every_operation_refuses_something_that_is_not_a_bridge_table() {
    for given in ["{}", "nil", "1", "io.stdout"] {
        let message = raises(&format!("bridge.table.subscribe({given}, function() end)"));
        assert!(
            message.contains("bridge.table.subscribe: expected a bridge.table"),
            "{given}: {message}"
        );
    }
}

#[test]
fn unsubscribe_takes_no_arguments() {
    let message = raises("bridge.table.subscribe(bridge.table(), print):unsubscribe(1)");
    assert!(
        message.contains("bridge.subscription.unsubscribe: expected no arguments, got 1"),
        "{message}"
    );
}

#[test]
fn unsubscribe_refuses_something_that_is_not_a_subscription() {
    for (given, named) in [
        ("{}", "table"),
        ("nil", "nil"),
        ("bridge.table()", "userdata"),
    ] {
        let message = raises(&format!(
            "local handle = bridge.table.subscribe(bridge.table(), print)
             handle.unsubscribe({given})"
        ));
        assert!(
            message.contains(&format!(
                "bridge.subscription.unsubscribe: expected a bridge.subscription, got {named}"
            )),
            "{given}: {message}"
        );
    }
}

// -- Telling in turn --------------------------------------------------------------------------

#[test]
fn a_subscriber_that_writes_has_its_write_told_to_everyone_before_the_one_it_was_told_of() {
    let got: String = eval(&format!(
        "{RECORD}
         local t = bridge.table()
         local first = {{}}
         bridge.table.subscribe(t, function(changes)
             first[#first + 1] = changes[1].key
             if changes[1].key == 'a' then t.b = t.a + 1 end
         end)
         bridge.table.subscribe(t, record)
         t.a = 1
         return table.concat(first, ' ') .. ' | ' .. told()"
    ));
    assert_eq!(got, "a b | [b:nil>2] [a:nil>1]");
}

#[test]
fn a_subscriber_added_while_a_change_is_told_hears_only_later_changes() {
    let got: String = eval(&format!(
        "{RECORD}
         local t = bridge.table()
         local added = false
         bridge.table.subscribe(t, function()
             if not added then
                 added = true
                 bridge.table.subscribe(t, record)
             end
         end)
         t.a = 1
         t.b = 2
         return told()"
    ));
    assert_eq!(got, "[b:nil>2]");
}

#[test]
fn a_subscriber_removed_while_a_change_is_told_is_not_called_again() {
    let got: String = eval(&format!(
        "{RECORD}
         local t = bridge.table()
         local second
         bridge.table.subscribe(t, function() second:unsubscribe() end)
         second = bridge.table.subscribe(t, record)
         t.a = 1
         t.b = 2
         return told()"
    ));
    assert_eq!(got, "");
}

#[test]
fn a_lua_subscriber_that_raises_does_not_fail_the_write_or_starve_the_rest() {
    // Run again as a child of its own, so that what it writes to standard error can be read. The
    // child is this same test, told by the variable that it is the child.
    const CHILD: &str = "AVARICE_BRIDGE_TEST_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let got: (String, i64) = eval(&format!(
            "{RECORD}
             local t = bridge.table()
             bridge.table.subscribe(t, function() error('the subscriber broke') end)
             bridge.table.subscribe(t, record)
             t.a = 1
             return told(), t.a"
        ));
        assert_eq!(got, ("[a:nil>1]".into(), 1));
        return;
    }
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "a_lua_subscriber_that_raises_does_not_fail_the_write_or_starve_the_rest",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD, "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(
        stdout.contains("1 passed"),
        "the child ran no test: {stdout}"
    );
    assert!(stderr.contains("the subscriber broke"), "{stderr}");
    assert!(!stdout.contains("the subscriber broke"), "{stdout}");
}

// -- Subscribing from Rust ---------------------------------------------------------------------

/// What a host's subscriber was told, a list per call, each change as `key:old>new`.
#[derive(Clone, Default)]
struct Told(Arc<Mutex<Vec<String>>>);

impl Told {
    fn record(&self, changes: &[bridge::Change]) {
        let shown = |v: &Value| v.to_string().unwrap();
        let parts: Vec<String> = changes
            .iter()
            .map(|c| format!("{}:{}>{}", shown(&c.key), shown(&c.old), shown(&c.new)))
            .collect();
        self.0
            .lock()
            .unwrap()
            .push(format!("[{}]", parts.join(",")));
    }

    fn get(&self) -> String {
        self.0.lock().unwrap().join(" ")
    }
}

/// A host module of the test's own, `host`, which receives Bridge tables from the script:
/// `host.watch(t)` subscribes, recording into `told`; `host.unwatch()` drops every subscription;
/// `host.keep(t)` keeps a handle; `host.set(k, v)` and `host.get(k)` write and read through the
/// last one kept or watched.
struct Host {
    rt: Runtime,
    told: Told,
    kept: Arc<Mutex<Option<bridge::Table>>>,
    subscriptions: Arc<Mutex<Vec<bridge::Subscription>>>,
}

impl Host {
    fn new() -> Self {
        Host::on(Runtime::new(Profile::Trusted).unwrap())
    }

    fn on(rt: Runtime) -> Self {
        let host = Host {
            rt,
            told: Told::default(),
            kept: Arc::default(),
            subscriptions: Arc::default(),
        };
        let lua = host.rt.lua();
        let module = lua.create_table().unwrap();
        let (told, kept, subscriptions) = (
            host.told.clone(),
            Arc::clone(&host.kept),
            Arc::clone(&host.subscriptions),
        );
        module
            .set(
                "watch",
                lua.create_function(move |_, t: bridge::Table| {
                    let told = told.clone();
                    let subscription = t.subscribe(move |_, changes| told.record(changes));
                    subscriptions.lock().unwrap().push(subscription);
                    *kept.lock().unwrap() = Some(t);
                    Ok(())
                })
                .unwrap(),
            )
            .unwrap();
        let subscriptions = Arc::clone(&host.subscriptions);
        module
            .set(
                "unwatch",
                lua.create_function(move |_, ()| {
                    subscriptions.lock().unwrap().clear();
                    Ok(())
                })
                .unwrap(),
            )
            .unwrap();
        let kept = Arc::clone(&host.kept);
        module
            .set(
                "keep",
                lua.create_function(move |_, t: bridge::Table| {
                    *kept.lock().unwrap() = Some(t);
                    Ok(())
                })
                .unwrap(),
            )
            .unwrap();
        let kept = Arc::clone(&host.kept);
        module
            .set(
                "set",
                lua.create_function(move |_, (k, v): (Value, Value)| {
                    let t = kept.lock().unwrap().clone().unwrap();
                    t.set(k, v)
                })
                .unwrap(),
            )
            .unwrap();
        let kept = Arc::clone(&host.kept);
        module
            .set(
                "get",
                lua.create_function(move |_, k: Value| {
                    let t = kept.lock().unwrap().clone().unwrap();
                    t.get::<Value>(k)
                })
                .unwrap(),
            )
            .unwrap();
        host.rt.register_module("host", module).unwrap();
        host
    }

    fn eval<R: mlua::FromLuaMulti>(&self, body: &str) -> R {
        eval_in(&self.rt, &format!("local host = require('host')\n{body}"))
    }

    /// The handle `host` last kept, from outside any Lua call.
    fn kept(&self) -> bridge::Table {
        self.kept.lock().unwrap().clone().unwrap()
    }
}

#[test]
fn a_host_function_receives_a_bridge_table_and_is_told_each_write() {
    let host = Host::new();
    host.eval::<()>(
        "local t = bridge.table({ a = 1 })
         host.watch(t)
         t.a = 2
         t.b = 'x'
         t.a = nil",
    );
    assert_eq!(host.told.get(), "[a:1>2] [b:nil>x] [a:2>nil]");
}

#[test]
fn a_host_function_refuses_what_is_not_a_bridge_table() {
    let host = Host::new();
    let message: String = host.eval(
        "local ok, err = pcall(host.watch, {})
         assert(not ok)
         return tostring(err)",
    );
    assert!(message.contains("bridge::Table"), "{message}");
}

#[test]
fn a_write_from_rust_tells_lua_subscribers_inside_that_call_and_rust_ones_too() {
    let host = Host::new();
    let got: (String, String) = host.eval(&format!(
        "{RECORD}
         local t = bridge.table()
         host.watch(t)
         bridge.table.subscribe(t, record)
         host.set('k', 'v')
         return told(), t.k"
    ));
    assert_eq!(got, ("[k:nil>v]".into(), "v".into()));
    assert_eq!(host.told.get(), "[k:nil>v]");
}

#[test]
fn a_write_from_rust_outside_lua_tells_lua_subscribers() {
    let host = Host::new();
    host.eval::<()>(&format!(
        "{RECORD}
         log_of = told
         local t = bridge.table()
         host.keep(t)
         bridge.table.subscribe(t, record)
         held = t"
    ));
    host.kept().set("n", 3).unwrap();
    let got: String = host.eval("return log_of()");
    assert_eq!(got, "[n:nil>3]");
    assert_eq!(host.kept().get::<i64>("n").unwrap(), 3);
}

#[test]
fn a_read_from_rust_sees_what_lua_wrote() {
    let host = Host::new();
    let got: (String, bool) = host.eval(
        "local t = bridge.table()
         host.keep(t)
         t.name = 'x'
         return host.get('name'), host.get('missing') == nil",
    );
    assert_eq!(got, ("x".into(), true));
}

#[test]
fn a_rust_subscriber_may_write_to_the_table_it_is_told_of() {
    let rt = Runtime::new(Profile::Trusted).unwrap();
    let lua = rt.lua();
    let subscriptions: Arc<Mutex<Vec<bridge::Subscription>>> = Arc::default();
    let normalise = lua
        .create_function(move |_, t: bridge::Table| {
            let handle = t.clone();
            let subscription = t.subscribe(move |_, changes| {
                for change in changes {
                    let Value::String(s) = &change.new else {
                        continue;
                    };
                    let upper = s.to_str().unwrap().to_uppercase();
                    if *s.to_str().unwrap() != *upper {
                        handle.set(change.key.clone(), upper).unwrap();
                    }
                }
            });
            subscriptions.lock().unwrap().push(subscription);
            Ok(())
        })
        .unwrap();
    let module = lua.create_table().unwrap();
    module.set("normalise", normalise).unwrap();
    rt.register_module("host", module).unwrap();
    let got: (String, String) = eval_in(
        &rt,
        &format!(
            "{RECORD}
             local t = bridge.table()
             require('host').normalise(t)
             bridge.table.subscribe(t, record)
             t.name = 'ada'
             return t.name, told()"
        ),
    );
    assert_eq!(got, ("ADA".into(), "[name:ada>ADA] [name:nil>ada]".into()));
}

#[test]
fn dropping_the_rust_subscription_stops_its_notifications() {
    let host = Host::new();
    host.eval::<()>(
        "local t = bridge.table()
         host.watch(t)
         t.a = 1
         host.unwatch()
         t.b = 2",
    );
    assert_eq!(host.told.get(), "[a:nil>1]");
}

#[test]
fn a_rust_subscriber_dropped_while_a_change_is_told_is_not_called_again() {
    let host = Host::new();
    host.eval::<()>(
        "local t = bridge.table()
         bridge.table.subscribe(t, function() host.unwatch() end)
         host.watch(t)
         t.a = 1
         t.b = 2",
    );
    assert_eq!(host.told.get(), "");
}

#[test]
fn a_write_from_another_thread_tells_lua_and_rust_subscribers() {
    let host = Host::new();
    host.eval::<()>(&format!(
        "{RECORD}
         log_of = told
         local t = bridge.table()
         host.watch(t)
         bridge.table.subscribe(t, record)
         held = t"
    ));
    let t = host.kept();
    std::thread::spawn(move || t.set("n", 3).unwrap())
        .join()
        .unwrap();
    let got: String = host.eval("return log_of()");
    assert_eq!(got, "[n:nil>3]");
    assert_eq!(host.told.get(), "[n:nil>3]");
}

#[test]
fn dropping_a_subscription_whose_subscriber_holds_another_does_not_deadlock() {
    let rt = Runtime::new(Profile::Trusted).unwrap();
    let held: Arc<Mutex<Option<bridge::Subscription>>> = Arc::default();
    let tangle = {
        let held = Arc::clone(&held);
        rt.lua()
            .create_function(move |_, t: bridge::Table| {
                let inner = t.subscribe(|_, _| {});
                let outer = t.subscribe(move |_, _| {
                    let _ = &inner;
                });
                *held.lock().unwrap() = Some(outer);
                Ok(())
            })
            .unwrap()
    };
    let module = rt.lua().create_table().unwrap();
    module.set("tangle", tangle).unwrap();
    rt.register_module("host", module).unwrap();
    eval_in::<()>(&rt, "held = bridge.table() require('host').tangle(held)");

    let outer = held.lock().unwrap().take().unwrap();
    let (done, finished) = mpsc::channel();
    std::thread::spawn(move || {
        drop(outer);
        done.send(()).unwrap();
    });
    finished
        .recv_timeout(Duration::from_secs(5))
        .expect("dropping the subscription deadlocked");
}

#[test]
fn a_cancel_inside_a_lua_subscriber_ends_the_write_and_tells_no_one_else() {
    let cancel = CancelHandle::new();
    let rt = Runtime::builder(Profile::Trusted)
        .cancel_handle(cancel.clone())
        .check_interval(100)
        .build()
        .unwrap();
    let stop = rt
        .lua()
        .create_function(move |_, ()| {
            cancel.cancel();
            Ok(())
        })
        .unwrap();
    rt.lua().globals().set("stop", stop).unwrap();
    let host = Host::on(rt);
    let err = host
        .rt
        .block_on(host.rt.eval::<()>(
            "local bridge = require('bridge')
             local host = require('host')
             local t = bridge.table()
             bridge.table.subscribe(t, function() stop() while true do end end)
             host.watch(t)
             t.a = 1",
            "=test",
        ))
        .unwrap_err();
    let Error::Lua(err) = err else {
        panic!("expected a Lua error, got {err:?}");
    };
    assert!(was_cancelled(&err), "{err}");
    assert_eq!(
        host.told.get(),
        "",
        "a subscriber was told after the cancel"
    );
}

// -- Batching ----------------------------------------------------------------------------------

#[test]
fn a_batch_tells_its_writes_as_one_list_in_write_order_when_it_returns() {
    let host = Host::new();
    let got: (String, String) = host.eval(&format!(
        "{RECORD}
         local t = bridge.table({{ a = 1 }})
         host.watch(t)
         bridge.table.subscribe(t, record)
         local during
         bridge.table.batch(t, function()
             t.a = 2
             t.b = 'x'
             t.a = 3
             during = told()
         end)
         return during, told()"
    ));
    assert_eq!(got, ("".into(), "[a:1>2,b:nil>x,a:2>3]".into()));
    assert_eq!(host.told.get(), "[a:1>2,b:nil>x,a:2>3]");
}

#[test]
fn the_table_library_writes_inside_a_batch_join_it() {
    let got: String = eval(&format!(
        "{RECORD}
         local t = bridge.table({{ 'c', 'a' }})
         bridge.table.subscribe(t, record)
         bridge.table.batch(t, function()
             table.insert(t, 'b')
             table.sort(t)
         end)
         return table.concat(t, ',') .. ' ' .. #log .. ' ' .. told()"
    ));
    // Which slots `table.sort` writes, and in what order, is up to its algorithm: what matters is
    // that they come after the insert, in the same one list.
    assert!(got.starts_with("a,b,c 1 [3:nil>b,"), "{got}");
    assert!(got.ends_with(']') && got.matches('[').count() == 1, "{got}");
}

#[test]
fn a_write_from_rust_inside_a_batch_joins_it() {
    let host = Host::new();
    let got: String = host.eval(&format!(
        "{RECORD}
         local t = bridge.table()
         host.watch(t)
         bridge.table.subscribe(t, record)
         bridge.table.batch(t, function()
             t.a = 1
             host.set('b', 2)
         end)
         return told()"
    ));
    assert_eq!(got, "[a:nil>1,b:nil>2]");
    assert_eq!(host.told.get(), "[a:nil>1,b:nil>2]");
}

#[test]
fn a_nested_batch_tells_nothing_until_the_outermost_ends() {
    let got: (String, String) = eval(&format!(
        "{RECORD}
         local t = bridge.table()
         bridge.table.subscribe(t, record)
         local after_inner
         bridge.table.batch(t, function()
             t.a = 1
             bridge.table.batch(t, function()
                 t.b = 2
             end)
             after_inner = told()
             t.c = 3
         end)
         return after_inner, told()"
    ));
    assert_eq!(got, ("".into(), "[a:nil>1,b:nil>2,c:nil>3]".into()));
}

#[test]
fn a_batch_with_no_writes_tells_nothing() {
    let got: String = eval(&format!(
        "{RECORD}
         local t = bridge.table({{ a = 1 }})
         bridge.table.subscribe(t, record)
         bridge.table.batch(t, function() local _ = t.a end)
         return told()"
    ));
    assert_eq!(got, "");
}

#[test]
fn a_batch_that_raises_keeps_and_tells_its_writes_then_raises_again() {
    let got: (String, bool, String, i64) = eval(&format!(
        "{RECORD}
         local t = bridge.table()
         bridge.table.subscribe(t, record)
         local ok, err = pcall(bridge.table.batch, t, function()
             t.a = 1
             error('the batch broke')
         end)
         local told_by_then = told()
         local raised = {{}}
         local _, again = pcall(bridge.table.batch, t, function() error(raised) end)
         return told_by_then, not ok and rawequal(again, raised), tostring(err), t.a"
    ));
    assert_eq!(got.0, "[a:nil>1]");
    assert!(got.1, "the error was not raised again as it was raised");
    assert!(got.2.contains("the batch broke"), "{}", got.2);
    assert_eq!(got.3, 1);
}

#[test]
fn a_write_by_a_subscriber_told_of_a_batch_is_told_on_its_own() {
    let got: String = eval(&format!(
        "{RECORD}
         local t = bridge.table()
         bridge.table.subscribe(t, function(changes)
             if changes[1].key == 'a' then t.c = 3 end
         end)
         bridge.table.subscribe(t, record)
         bridge.table.batch(t, function()
             t.a = 1
             t.b = 2
         end)
         return told()"
    ));
    assert_eq!(got, "[c:nil>3] [a:nil>1,b:nil>2]");
}

#[test]
fn a_cancel_inside_a_batch_ends_it_as_a_cancel() {
    let cancel = CancelHandle::new();
    let rt = Runtime::builder(Profile::Trusted)
        .cancel_handle(cancel.clone())
        .check_interval(100)
        .build()
        .unwrap();
    let stop = rt
        .lua()
        .create_function(move |_, ()| {
            cancel.cancel();
            Ok(())
        })
        .unwrap();
    rt.lua().globals().set("stop", stop).unwrap();
    let err = rt
        .block_on(rt.eval::<()>(
            "local bridge = require('bridge')
             local t = bridge.table()
             bridge.table.batch(t, function() t.a = 1 stop() while true do end end)",
            "=test",
        ))
        .unwrap_err();
    let Error::Lua(err) = err else {
        panic!("expected a Lua error, got {err:?}");
    };
    assert!(was_cancelled(&err), "{err}");
}

#[test]
fn table_insert_at_the_front_outside_a_batch_tells_each_shifted_slot_on_its_own() {
    let got: String = eval(&format!(
        "{RECORD}
         local t = bridge.table({{ 'b', 'c' }})
         bridge.table.subscribe(t, record)
         table.insert(t, 1, 'a')
         return told()"
    ));
    assert_eq!(got, "[3:nil>c] [2:c>b] [1:b>a]");
}

#[test]
fn bridge_insert_in_the_middle_shifts_the_sequence_as_one_list() {
    let got: String = eval(&format!(
        "{RECORD}
         local t = bridge.table({{ 'a', 'c', 'd' }})
         bridge.table.subscribe(t, record)
         bridge.table.insert(t, 2, 'b')
         return table.concat(t, ',') .. ' ' .. told()"
    ));
    assert_eq!(got, "a,b,c,d [4:nil>d,3:d>c,2:c>b]");
}

#[test]
fn bridge_insert_without_a_position_appends() {
    let got: String = eval(&format!(
        "{RECORD}
         local t = bridge.table({{ 'a' }})
         bridge.table.subscribe(t, record)
         bridge.table.insert(t, 'b')
         return table.concat(t, ',') .. ' ' .. told()"
    ));
    assert_eq!(got, "a,b [2:nil>b]");
}

#[test]
fn bridge_remove_in_the_middle_shifts_the_sequence_as_one_list_and_gives_the_value() {
    let got: String = eval(&format!(
        "{RECORD}
         local t = bridge.table({{ 'a', 'b', 'c' }})
         bridge.table.subscribe(t, record)
         local removed = bridge.table.remove(t, 2)
         return removed .. ' ' .. table.concat(t, ',') .. ' ' .. #t .. ' ' .. told()"
    ));
    assert_eq!(got, "b a,c 2 [2:b>c,3:c>nil]");
}

#[test]
fn bridge_remove_without_a_position_removes_the_last() {
    let got: (String, String, String) = eval(&format!(
        "{RECORD}
         local t = bridge.table({{ 'a', 'b' }})
         bridge.table.subscribe(t, record)
         local removed = bridge.table.remove(t)
         return removed, table.concat(t, ','), told()"
    ));
    assert_eq!(got, ("b".into(), "a".into(), "[2:b>nil]".into()));
}

#[test]
fn bridge_insert_and_remove_inside_a_batch_join_it() {
    let got: String = eval(&format!(
        "{RECORD}
         local t = bridge.table({{ 'a', 'b' }})
         bridge.table.subscribe(t, record)
         bridge.table.batch(t, function()
             bridge.table.insert(t, 1, 'z')
             bridge.table.remove(t, 1)
         end)
         return told()"
    ));
    assert_eq!(got, "[3:nil>b,2:b>a,1:a>z,1:z>a,2:a>b,3:b>nil]");
}

#[test]
fn bridge_insert_and_remove_take_the_positions_table_insert_and_remove_do() {
    // The same calls on a plain table through the `table` library, and on a Bridge table through
    // `bridge.table`, give the same contents and the same values back.
    let got: (String, String) = eval(
        "local function run(insert, remove, t)
             local out = {}
             insert(t, 1, 'x')
             insert(t, #t + 1, 'y')
             insert(t, 'z')
             out[#out + 1] = tostring(remove(t, 1))
             out[#out + 1] = tostring(remove(t))
             out[#out + 1] = tostring(remove(t, #t + 1))
             out[#out + 1] = tostring(remove(t, '2'))
             local all = {}
             for i = 1, #t do all[i] = tostring(t[i]) end
             return table.concat(out, ',') .. ' | ' .. table.concat(all, ',')
         end
         local plain = run(table.insert, table.remove, { 'a', 'b' })
         local bridged = run(bridge.table.insert, bridge.table.remove, bridge.table({ 'a', 'b' }))
         return plain, bridged",
    );
    assert_eq!(got.0, got.1);
    assert_eq!(got.0, "x,z,nil,b | a,y");
}

#[test]
fn bridge_insert_and_remove_refuse_a_position_out_of_bounds() {
    for call in [
        "bridge.table.insert(bridge.table({ 'a' }), 3, 'x')",
        "bridge.table.insert(bridge.table({ 'a' }), 0, 'x')",
        "bridge.table.remove(bridge.table({ 'a' }), 3)",
        "bridge.table.remove(bridge.table({ 'a' }), -1)",
    ] {
        let message = raises(call);
        assert!(
            message.contains("position out of bounds"),
            "{call}: {message}"
        );
    }
}

#[test]
fn bridge_insert_and_remove_check_their_arguments() {
    for (call, expected) in [
        (
            "bridge.table.insert({}, 'x')",
            "bridge.table.insert: expected a bridge.table, got table",
        ),
        (
            "bridge.table.insert(bridge.table())",
            "bridge.table.insert: wrong number of arguments",
        ),
        (
            "bridge.table.insert(bridge.table(), 1, 'x', 'y')",
            "bridge.table.insert: wrong number of arguments",
        ),
        (
            "bridge.table.insert(bridge.table(), 'one', 'x')",
            "bridge.table.insert: expected an integer position, got string",
        ),
        (
            "bridge.table.remove({})",
            "bridge.table.remove: expected a bridge.table, got table",
        ),
        (
            "bridge.table.remove(bridge.table(), 1.5)",
            "bridge.table.remove: expected an integer position, got number",
        ),
        (
            "bridge.table.remove(bridge.table(), 1, 2)",
            "bridge.table.remove: expected at most 2 arguments, got 3",
        ),
    ] {
        let message = raises(call);
        assert!(message.contains(expected), "{call}: {message}");
    }
}

#[test]
fn a_batch_that_runs_out_of_memory_ends_as_out_of_memory() {
    // With no subscriber, and with a Lua and a Rust one: each is told, or tries to be, before the
    // batch's own error goes on.
    for watched in [
        "",
        "host.watch(t) bridge.table.subscribe(t, function() end)",
    ] {
        let rt = Runtime::builder(Profile::Trusted)
            .memory_limit(8 * 1024 * 1024)
            .build()
            .unwrap();
        let host = Host::on(rt);
        let err = host
            .rt
            .block_on(host.rt.exec(
                format!(
                    "local bridge = require('bridge')
                     local host = require('host')
                     local t = bridge.table()
                     {watched}
                     bridge.table.batch(t, function()
                         local i = 0
                         while true do i = i + 1 t[i] = {{}} end
                     end)"
                ),
                "=test",
            ))
            .unwrap_err();
        let Error::Lua(err) = err else {
            panic!("expected a Lua error, got {err:?}");
        };
        assert!(
            was_out_of_memory(&err),
            "{watched}: expected OOM, got {err}"
        );
    }
}

#[test]
fn a_batch_whose_fn_yields_raises_that_it_cannot() {
    let got: String = eval(
        "local t = bridge.table()
         local inside = coroutine.wrap(function()
             return pcall(bridge.table.batch, t, function() coroutine.yield() end)
         end)
         local ok, err = inside()
         assert(not ok)
         return tostring(err)",
    );
    assert_eq!(got, "bridge.table.batch: fn cannot yield");
}

#[test]
fn a_batch_past_ten_thousand_changes_reaches_a_rust_subscriber_in_order_as_several_calls() {
    let rt = Runtime::new(Profile::Trusted).unwrap();
    let calls: Arc<Mutex<Vec<Vec<i64>>>> = Arc::default();
    let subscriptions: Arc<Mutex<Vec<bridge::Subscription>>> = Arc::default();
    let watch = {
        let (calls, subscriptions) = (Arc::clone(&calls), Arc::clone(&subscriptions));
        rt.lua()
            .create_function(move |_, t: bridge::Table| {
                let calls = Arc::clone(&calls);
                let subscription = t.subscribe(move |_, changes| {
                    let keys = changes.iter().map(|c| c.key.as_i64().unwrap()).collect();
                    calls.lock().unwrap().push(keys);
                });
                subscriptions.lock().unwrap().push(subscription);
                Ok(())
            })
            .unwrap()
    };
    let module = rt.lua().create_table().unwrap();
    module.set("watch", watch).unwrap();
    rt.register_module("host", module).unwrap();
    // Each value is a table, so that every change holds a value mlua must keep a reference to.
    let lua_lists: String = eval_in(
        &rt,
        "local t = bridge.table()
         require('host').watch(t)
         local lists = {}
         bridge.table.subscribe(t, function(changes) lists[#lists + 1] = #changes end)
         for _, n in ipairs({ 10000, 10001 }) do
             bridge.table.batch(t, function()
                 for i = 1, n do t[i] = {} end
             end)
         end
         return table.concat(lists, ' ')",
    );
    assert_eq!(lua_lists, "10000 10001");
    let calls = calls.lock().unwrap();
    let sizes: Vec<usize> = calls.iter().map(Vec::len).collect();
    assert_eq!(sizes, [10_000, 10_000, 1]);
    let second: Vec<i64> = calls[1..].concat();
    assert_eq!(second, (1..=10_001).collect::<Vec<i64>>());
}

#[test]
fn batch_checks_its_arguments() {
    for (call, expected) in [
        (
            "bridge.table.batch({}, function() end)",
            "bridge.table.batch: expected a bridge.table, got table",
        ),
        (
            "bridge.table.batch(bridge.table(), 'x')",
            "bridge.table.batch: expected a function, got string",
        ),
        (
            "bridge.table.batch(bridge.table(), print, 1)",
            "bridge.table.batch: expected at most 2 arguments, got 3",
        ),
    ] {
        let message = raises(call);
        assert!(message.contains(expected), "{call}: {message}");
    }
}

// -- What it is --------------------------------------------------------------------------------

#[test]
fn tostring_names_it_and_gives_an_address() {
    let got: (String, bool) = eval(
        "local t, u = bridge.table(), bridge.table()
         return tostring(t), tostring(t) ~= tostring(u)",
    );
    assert!(got.0.starts_with("bridge.table: 0x"), "{}", got.0);
    assert!(got.1, "two Bridge tables have the same address");
}

#[test]
fn raw_access_and_type_see_a_userdata() {
    let got: (String, bool) = eval(
        "local t = bridge.table({ a = 1 })
         return type(t), pcall(rawget, t, 'a')",
    );
    assert_eq!(got, ("userdata".into(), false));
}

// -- Collection --------------------------------------------------------------------------------

#[test]
fn a_bridge_table_that_refers_to_itself_is_collected() {
    // Made in a function of their own, so that no register of the chunk still holds one.
    let got: bool = eval(
        "local weak = setmetatable({}, { __mode = 'v' })
         local function make()
             local direct = bridge.table()
             direct.self = direct
             local captured = bridge.table()
             captured.fn = function() return captured end
             weak.direct, weak.captured = direct, captured
         end
         make()
         assert(weak.direct and weak.captured)
         collectgarbage()
         collectgarbage()
         return weak.direct == nil and weak.captured == nil",
    );
    assert!(got, "a Bridge table in a cycle was kept alive");
}

#[test]
fn a_bridge_table_its_own_lua_subscriber_captures_is_collected() {
    let got: bool = eval(
        "local weak = setmetatable({}, { __mode = 'k' })
         local function make()
             local t = bridge.table()
             local handle
             handle = bridge.table.subscribe(t, function() return t, handle end)
             weak[t] = true
         end
         make()
         assert(next(weak))
         collectgarbage()
         collectgarbage()
         return next(weak) == nil",
    );
    assert!(got, "a Bridge table its subscriber captures was kept alive");
}

#[test]
fn the_hosts_handle_and_subscription_do_not_keep_a_bridge_table_alive() {
    let host = Host::new();
    let got: bool = host.eval(
        "local weak = setmetatable({}, { __mode = 'k' })
         local function make()
             local t = bridge.table({ a = 1 })
             host.watch(t)
             weak[t] = true
         end
         make()
         assert(next(weak))
         collectgarbage()
         collectgarbage()
         return next(weak) == nil",
    );
    assert!(got, "the host's hold kept a Bridge table alive");
    let message = host.kept().set("a", 2).unwrap_err().to_string();
    assert!(message.contains("collected"), "{message}");
    assert!(host.kept().get::<Value>("a").is_err());
}

// -- Who has it --------------------------------------------------------------------------------

#[test]
fn trusted_mode_registers_bridge() {
    let rt = Runtime::new(Profile::Trusted).unwrap();
    assert!(rt.has_module("bridge"));
}

#[test]
fn sandbox_mode_does_not_register_bridge() {
    let rt = Runtime::new(Profile::Sandbox).unwrap();
    assert!(!rt.has_module("bridge"));
    let message: String = rt
        .block_on(rt.eval(
            "local ok, err = pcall(require, 'bridge') assert(not ok) return tostring(err)",
            "=test",
        ))
        .unwrap();
    assert!(message.contains("bridge"), "{message}");
}

#[test]
fn with_std_modules_adds_bridge_to_a_sandbox() {
    let rt = Runtime::builder(Profile::Sandbox)
        .with_std_modules(StdModules::BRIDGE)
        .build()
        .unwrap();
    let got: i64 = eval_in(
        &rt,
        "local t = bridge.table()
         t.x = 1
         return t.x",
    );
    assert_eq!(got, 1);
}
