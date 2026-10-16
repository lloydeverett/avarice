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

    /// Records a Bridge cell's set, as `old>new`.
    fn record_set(&self, old: &Value, new: &Value) {
        let shown = |v: &Value| v.to_string().unwrap();
        self.0
            .lock()
            .unwrap()
            .push(format!("{}>{}", shown(old), shown(new)));
    }

    /// Records that the table closed, as `closed`.
    fn closed(&self) {
        self.0.lock().unwrap().push("closed".to_owned());
    }

    fn get(&self) -> String {
        self.0.lock().unwrap().join(" ")
    }
}

/// A host module of the test's own, `host`, which receives Bridge tables from the script:
/// `host.watch(t)` subscribes, recording into `told`, its close as `closed`; `host.unwatch()` drops
/// every subscription; `host.keep(t)` keeps a handle; `host.set(k, v)`, `host.get(k)` and
/// `host.close()` write, read and close through the last one kept or watched.
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
                    let (on_change, on_close) = (told.clone(), told.clone());
                    let subscription = t.subscribe_with_close(
                        move |_, changes| on_change.record(changes),
                        move || on_close.closed(),
                    );
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
        let kept = Arc::clone(&host.kept);
        module
            .set(
                "close",
                lua.create_function(move |_, ()| {
                    let t = kept.lock().unwrap().clone().unwrap();
                    t.close()
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
    // Its close, should the table have been collected since, is not a change.
    let told = host.told.get();
    assert!(
        !told.contains('['),
        "a subscriber was told after the cancel: {told}"
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

// -- Closing -----------------------------------------------------------------------------------

#[test]
fn closing_from_lua_runs_each_lua_close_callback_once_and_a_second_close_does_nothing() {
    let got: String = eval(
        "local log = {}
         local t = bridge.table()
         bridge.table.subscribe(t, function() end, function() log[#log + 1] = 'first' end)
         bridge.table.subscribe(t, function() end)
         bridge.table.subscribe(t, function() end, function() log[#log + 1] = 'second' end)
         bridge.table.close(t)
         bridge.table.close(t)
         return table.concat(log, ' ')",
    );
    assert_eq!(got, "first second");
}

#[test]
fn closing_from_lua_runs_each_rust_close_callback_once() {
    let host = Host::new();
    host.eval::<()>(
        "local t = bridge.table()
         host.watch(t)
         t.a = 1
         bridge.table.close(t)
         bridge.table.close(t)",
    );
    assert_eq!(host.told.get(), "[a:nil>1] closed");
}

#[test]
fn closing_from_rust_runs_lua_and_rust_close_callbacks_once_in_the_order_they_subscribed() {
    let host = Host::new();
    host.eval::<()>(
        "log = {}
         local t = bridge.table()
         bridge.table.subscribe(t, print, function() log[#log + 1] = 'lua' end)
         host.watch(t)
         held = t",
    );
    host.kept().close().unwrap();
    host.kept().close().unwrap();
    let got: String = host.eval("return table.concat(log, ' ')");
    assert_eq!(got, "lua");
    assert_eq!(host.told.get(), "closed");
    // From inside Lua as well.
    let got: String = host.eval(
        "local log = {}
         local t = bridge.table()
         host.watch(t)
         bridge.table.subscribe(t, print, function() log[#log + 1] = 'lua' end)
         host.close()
         host.close()
         return table.concat(log, ' ')",
    );
    assert_eq!(got, "lua");
    assert_eq!(host.told.get(), "closed closed");
}

#[test]
fn a_closed_bridge_table_can_be_read_and_walked() {
    let got: (i64, i64, i64, i64) = eval(
        "local t = bridge.table({ 10, 20, name = 'x' })
         bridge.table.close(t)
         local n = 0
         for _ in pairs(t) do n = n + 1 end
         local sum = 0
         for _, v in ipairs(t) do sum = sum + v end
         return t[2], #t, n, sum",
    );
    assert_eq!(got, (20, 2, 3, 30));
}

#[test]
fn a_write_to_a_closed_bridge_table_raises_and_changes_nothing() {
    for (write, expected) in [
        ("t.a = 2", "bridge.table: the Bridge table is closed"),
        ("t.new = 1", "bridge.table: the Bridge table is closed"),
        (
            "table.insert(t, 3)",
            "bridge.table: the Bridge table is closed",
        ),
        (
            "bridge.table.insert(t, 3)",
            "bridge.table.insert: the Bridge table is closed",
        ),
        (
            "bridge.table.remove(t)",
            "bridge.table.remove: the Bridge table is closed",
        ),
        (
            "bridge.table.batch(t, function() end)",
            "bridge.table.batch: the Bridge table is closed",
        ),
        (
            "bridge.table.subscribe(t, print)",
            "bridge.table.subscribe: the Bridge table is closed",
        ),
    ] {
        let message = raises(&format!(
            "local t = bridge.table({{ a = 1 }})
             bridge.table.close(t)
             local ok, err = pcall(function() {write} end)
             assert(t.a == 1 and t.new == nil and #t == 0, 'the write changed it')
             error(err, 0)"
        ));
        assert!(message.contains(expected), "{write}: {message}");
    }
}

#[test]
fn a_write_from_rust_to_a_closed_bridge_table_is_an_error_and_a_read_is_not() {
    let host = Host::new();
    host.eval::<()>(
        "held = bridge.table({ a = 1 })
         host.keep(held)
         bridge.table.close(held)",
    );
    let message = host.kept().set("a", 2).unwrap_err().to_string();
    assert!(message.contains("the Bridge table is closed"), "{message}");
    assert_eq!(host.kept().get::<i64>("a").unwrap(), 1);
}

#[test]
fn a_close_callback_that_unsubscribes_a_later_subscriber_keeps_it_from_being_told() {
    let got: String = eval(
        "local log = {}
         local t = bridge.table()
         local second
         bridge.table.subscribe(t, print, function()
             log[#log + 1] = 'first'
             second:unsubscribe()
         end)
         second = bridge.table.subscribe(t, print, function() log[#log + 1] = 'second' end)
         bridge.table.close(t)
         return table.concat(log, ' ')",
    );
    assert_eq!(got, "first");
}

#[test]
fn a_rust_close_callback_may_drop_its_own_subscription_and_another() {
    let rt = Runtime::new(Profile::Trusted).unwrap();
    let log: Arc<Mutex<Vec<&str>>> = Arc::default();
    let tangle = {
        let log = Arc::clone(&log);
        rt.lua()
            .create_function(move |_, t: bridge::Table| {
                let held: Arc<Mutex<Vec<bridge::Subscription>>> = Arc::default();
                let (first_log, second_log) = (Arc::clone(&log), Arc::clone(&log));
                let first_held = Arc::clone(&held);
                let first = t.subscribe_with_close(
                    |_, _| {},
                    move || {
                        first_log.lock().unwrap().push("first");
                        // Its own and the second, each dropping under the module's locks.
                        first_held.lock().unwrap().clear();
                    },
                );
                let second = t.subscribe_with_close(
                    |_, _| {},
                    move || second_log.lock().unwrap().push("second"),
                );
                held.lock().unwrap().extend([first, second]);
                Ok(())
            })
            .unwrap()
    };
    rt.lua().globals().set("tangle", tangle).unwrap();
    let (done, finished) = mpsc::channel();
    std::thread::spawn(move || {
        eval_in::<()>(
            &rt,
            "local t = bridge.table()
             tangle(t)
             bridge.table.close(t)",
        );
        done.send(()).unwrap();
    });
    finished
        .recv_timeout(Duration::from_secs(5))
        .expect("closing deadlocked");
    assert_eq!(log.lock().unwrap().join(" "), "first");
}

#[test]
fn closing_inside_a_batch_tells_its_writes_before_the_close_and_ends_it() {
    let host = Host::new();
    let got: (String, String) = host.eval(&format!(
        "{RECORD}
         local t = bridge.table()
         host.watch(t)
         bridge.table.subscribe(t, record, function() log[#log + 1] = 'closed' end)
         local ok, err = pcall(bridge.table.batch, t, function()
             t.a = 1
             t.b = 2
             bridge.table.close(t)
             t.c = 3
         end)
         assert(not ok)
         return told(), tostring(err)"
    ));
    assert_eq!(got.0, "[a:nil>1,b:nil>2] closed");
    assert!(got.1.contains("the Bridge table is closed"), "{}", got.1);
    assert_eq!(host.told.get(), "[a:nil>1,b:nil>2] closed");
}

#[test]
fn a_lua_close_callback_that_raises_does_not_fail_the_close_or_starve_the_rest() {
    // Run again as a child of its own, as the test of a Lua subscriber that raises is, so that what
    // it writes to standard error can be read.
    const CHILD: &str = "AVARICE_BRIDGE_TEST_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let host = Host::new();
        let got: String = host.eval(
            "local log = {}
             local t = bridge.table()
             bridge.table.subscribe(t, print, function() error('the close callback broke') end)
             host.watch(t)
             bridge.table.subscribe(t, print, function() log[#log + 1] = 'after' end)
             bridge.table.close(t)
             return table.concat(log, ' ')",
        );
        assert_eq!(got, "after");
        assert_eq!(host.told.get(), "closed");
        return;
    }
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "a_lua_close_callback_that_raises_does_not_fail_the_close_or_starve_the_rest",
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
    assert!(stderr.contains("the close callback broke"), "{stderr}");
}

#[test]
fn a_cancel_inside_a_lua_close_callback_ends_the_close_but_still_tells_rust() {
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
             later = false
             held = bridge.table()
             bridge.table.subscribe(held, print, function() stop() while true do end end)
             host.watch(held)
             bridge.table.subscribe(held, print, function() later = true end)
             bridge.table.close(held)",
            "=test",
        ))
        .unwrap_err();
    let Error::Lua(err) = err else {
        panic!("expected a Lua error, got {err:?}");
    };
    assert!(was_cancelled(&err), "{err}");
    assert_eq!(host.told.get(), "closed");
    let later: bool = host.rt.lua().globals().get("later").unwrap();
    assert!(!later, "a Lua close callback was called after the cancel");
}

#[test]
fn subscribing_from_rust_to_a_closed_bridge_table_tells_the_close_at_once_and_nothing_else() {
    let host = Host::new();
    host.eval::<()>(
        "local t = bridge.table()
         bridge.table.close(t)
         host.watch(t)",
    );
    assert_eq!(host.told.get(), "closed");
}

#[test]
fn close_and_subscribe_check_their_arguments() {
    for (call, expected) in [
        (
            "bridge.table.close({})",
            "bridge.table.close: expected a bridge.table, got table",
        ),
        (
            "bridge.table.close(bridge.table(), 1)",
            "bridge.table.close: expected at most 1 argument, got 2",
        ),
        (
            "bridge.table.subscribe(bridge.table(), print, 'x')",
            "bridge.table.subscribe: expected a function for on_close, got string",
        ),
        (
            "bridge.table.subscribe(bridge.table(), print, print, 1)",
            "bridge.table.subscribe: expected at most 3 arguments, got 4",
        ),
    ] {
        let message = raises(call);
        assert!(message.contains(expected), "{call}: {message}");
    }
}

#[test]
fn a_close_while_a_change_is_told_lets_every_subscriber_hear_it_then_tells_the_close_last() {
    let host = Host::new();
    let got: String = host.eval(
        "local log = {}
         local t = bridge.table()
         bridge.table.subscribe(t, function(changes)
             log[#log + 1] = 'A:' .. changes[1].key
             bridge.table.close(t)
             local ok, err = pcall(function() t.b = 2 end)
             log[#log + 1] = ok and 'wrote' or tostring(err):match('the Bridge table is closed')
         end, function() log[#log + 1] = 'A closed' end)
         host.watch(t)
         bridge.table.subscribe(t, function(changes)
             log[#log + 1] = 'B:' .. changes[1].key
         end, function() log[#log + 1] = 'B closed' end)
         t.a = 1
         return table.concat(log, ', ')",
    );
    assert_eq!(
        got,
        "A:a, the Bridge table is closed, B:a, A closed, B closed"
    );
    assert_eq!(host.told.get(), "[a:nil>1] closed");
}

#[test]
fn a_close_inside_a_nested_tell_waits_for_the_outermost_tell_to_finish() {
    let got: String = eval(
        "local log = {}
         local t = bridge.table()
         bridge.table.subscribe(t, function(changes)
             if changes[1].key == 'a' then t.b = 2 end
         end)
         bridge.table.subscribe(t, function(changes)
             log[#log + 1] = 'C:' .. changes[1].key
             if changes[1].key == 'b' then bridge.table.close(t) end
         end, function() log[#log + 1] = 'C closed' end)
         t.a = 1
         return table.concat(log, ', ')",
    );
    assert_eq!(got, "C:b, C:a, C closed");
}

#[test]
fn a_subscriber_that_writes_while_it_hears_a_batch_flushed_by_close_is_refused() {
    let got: String = eval(
        "local log = {}
         local t = bridge.table()
         bridge.table.subscribe(t, function(changes)
             local ok, err = pcall(function() t.z = 1 end)
             local refused = tostring(err):match('[%w.]+: the Bridge table is closed')
             log[#log + 1] = #changes .. ' ' .. (ok and 'wrote' or refused)
         end, function() log[#log + 1] = 'closed' end)
         pcall(bridge.table.batch, t, function()
             t.a = 1
             t.b = 2
             bridge.table.close(t)
         end)
         return table.concat(log, ', ') .. ' | ' .. tostring(t.z)",
    );
    assert_eq!(
        got,
        "2 bridge.table: the Bridge table is closed, closed | nil"
    );
}

#[test]
fn a_limit_while_close_tells_a_batch_still_lets_go_of_the_lua_subscribers() {
    let cancel = CancelHandle::new();
    let rt = Runtime::builder(Profile::Trusted)
        .cancel_handle(cancel.clone())
        .check_interval(100)
        .build()
        .unwrap();
    let stop = {
        let cancel = cancel.clone();
        rt.lua()
            .create_function(move |_, ()| {
                cancel.cancel();
                Ok(())
            })
            .unwrap()
    };
    rt.lua().globals().set("stop", stop).unwrap();
    let host = Host::on(rt);
    let err = host
        .rt
        .block_on(host.rt.eval::<()>(
            "local bridge = require('bridge')
             local host = require('host')
             weak = setmetatable({}, { __mode = 'v' })
             held = bridge.table()
             host.watch(held)
             bridge.table.subscribe(held, function() stop() while true do end end)
             local function make()
                 local later = function() end
                 weak.later = later
                 bridge.table.subscribe(held, later, later)
             end
             make()
             bridge.table.batch(held, function()
                 held.a = 1
                 bridge.table.close(held)
             end)",
            "=test",
        ))
        .unwrap_err();
    let Error::Lua(err) = err else {
        panic!("expected a Lua error, got {err:?}");
    };
    assert!(was_cancelled(&err), "{err}");
    assert_eq!(host.told.get(), "[a:nil>1] closed");
    cancel.reset();
    let let_go: bool = host.eval(
        "collectgarbage()
         collectgarbage()
         return held ~= nil and weak.later == nil",
    );
    assert!(let_go, "the closed table still holds its Lua subscribers");
}

#[test]
fn a_rust_close_callback_that_panics_on_an_explicit_close_panics() {
    let host = Host::new();
    host.eval::<()>("held = bridge.table() host.keep(held)");
    let subscription = host
        .kept()
        .subscribe_with_close(|_, _| {}, || panic!("the close callback broke"));
    let kept = host.kept();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| kept.close()));
    assert!(result.is_err(), "the panic did not reach the closer");
    drop(subscription);
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

#[test]
fn collection_closes_a_bridge_table_telling_only_its_rust_close_callbacks() {
    let host = Host::new();
    let got: bool = host.eval(
        "lua_told = false
         local function make()
             local t = bridge.table()
             host.watch(t)
             bridge.table.subscribe(t, print, function() lua_told = true end)
         end
         make()
         collectgarbage()
         collectgarbage()
         return lua_told",
    );
    assert!(!got, "a Lua close callback was called by collection");
    assert_eq!(host.told.get(), "closed");
    host.kept().close().unwrap();
    assert_eq!(host.told.get(), "closed", "closing it again told it again");
}

#[test]
fn a_bridge_table_in_a_cycle_through_itself_or_its_subscriber_is_closed_when_collected() {
    let host = Host::new();
    host.eval::<()>(
        "local function make()
             local direct = bridge.table()
             direct.self = direct
             host.watch(direct)
             local captured = bridge.table()
             local handle
             handle = bridge.table.subscribe(captured, function() return captured, handle end)
             host.watch(captured)
         end
         make()
         collectgarbage()
         collectgarbage()",
    );
    assert_eq!(host.told.get(), "closed closed");
}

#[test]
fn dropping_the_runtime_with_an_open_bridge_table_runs_its_rust_close_callbacks() {
    let host = Host::new();
    // A close callback that drops a subscription to its own table, and reaches for the table.
    let reached: Arc<Mutex<Vec<String>>> = Arc::default();
    let tidy = {
        let reached = Arc::clone(&reached);
        let subscriptions = Arc::clone(&host.subscriptions);
        host.rt
            .lua()
            .create_function(move |_, t: bridge::Table| {
                let other = t.subscribe(|_, _| {});
                let reached = Arc::clone(&reached);
                let handle = t.clone();
                let subscription = t.subscribe_with_close(
                    |_, _| {},
                    move || {
                        drop(other);
                        let mut reached = reached.lock().unwrap();
                        reached.push(format!("set: {}", handle.set("a", 2).is_err()));
                        reached.push(format!("close: {:?}", handle.close().is_ok()));
                    },
                );
                subscriptions.lock().unwrap().push(subscription);
                Ok(())
            })
            .unwrap()
    };
    host.rt.lua().globals().set("tidy", tidy).unwrap();
    host.eval::<()>(
        "held = bridge.table({ a = 1 })
         host.watch(held)
         tidy(held)
         bridge.table.subscribe(held, print, function() error('not to be called') end)",
    );
    let Host {
        rt,
        told,
        subscriptions,
        ..
    } = host;
    drop(rt);
    assert_eq!(told.get(), "closed");
    drop(subscriptions);
    assert_eq!(reached.lock().unwrap().join(", "), "set: true, close: true");
}

#[test]
fn a_close_callback_may_drop_the_runtime_it_was_closed_from() {
    let rt: Arc<Mutex<Option<Runtime>>> =
        Arc::new(Mutex::new(Some(Runtime::new(Profile::Trusted).unwrap())));
    let (kept, subscription) = {
        let guard = rt.lock().unwrap();
        let runtime = guard.as_ref().unwrap();
        let t: bridge::Table = eval_in(runtime, "held = bridge.table() return held");
        let rt = Arc::clone(&rt);
        let subscription =
            t.subscribe_with_close(|_, _| {}, move || drop(rt.lock().unwrap().take()));
        (t, subscription)
    };
    kept.close().unwrap();
    assert!(
        rt.lock().unwrap().is_none(),
        "the close callback did not run"
    );
    drop(subscription);
    assert!(kept.get::<Value>("a").is_err());
}

/// What the close callbacks of [`tangled`] got from `other`, an error message each.
type Reached = Arc<Mutex<Vec<String>>>;

/// The subscriptions [`tangled`] made, kept so that they are told of the close.
type Held = Arc<Mutex<Vec<bridge::Subscription>>>;

/// A host module, `host`, whose `host.tangle(t, other)` subscribes to `t` with close callbacks
/// that panic, when `panics`, and that reach for `other` from Rust, recording what they get.
fn tangled(rt: &Runtime, panics: bool) -> (Reached, Held) {
    let reached: Reached = Arc::default();
    let subscriptions: Held = Arc::default();
    let tangle = {
        let (reached, subscriptions) = (Arc::clone(&reached), Arc::clone(&subscriptions));
        rt.lua()
            .create_function(move |_, (t, other): (bridge::Table, bridge::Table)| {
                if panics {
                    let panicking = t.subscribe_with_close(|_, _| {}, || panic!("on_close broke"));
                    subscriptions.lock().unwrap().push(panicking);
                }
                let reached = Arc::clone(&reached);
                let reaching = t.subscribe_with_close(
                    |_, _| {},
                    move || {
                        let mut reached = reached.lock().unwrap();
                        let shown = |e: mlua::Error| e.to_string();
                        reached.push(other.set("k", 1).map_err(shown).err().unwrap_or_default());
                        reached.push(
                            other
                                .get::<Value>("k")
                                .map_err(shown)
                                .err()
                                .unwrap_or_default(),
                        );
                        reached.push(other.close().map_err(shown).err().unwrap_or_default());
                    },
                );
                subscriptions.lock().unwrap().push(reaching);
                Ok(())
            })
            .unwrap()
    };
    let module = rt.lua().create_table().unwrap();
    module.set("tangle", tangle).unwrap();
    rt.register_module("host", module).unwrap();
    (reached, subscriptions)
}

const REFUSED: &str = "a Bridge table cannot be used from a close callback run by collection";

#[test]
fn a_close_callback_run_by_collection_cannot_call_into_lua() {
    let rt = Runtime::new(Profile::Trusted).unwrap();
    let (reached, _subscriptions) = tangled(&rt, false);
    let other_told: String = eval_in(
        &rt,
        "local log = {}
         other = bridge.table()
         bridge.table.subscribe(other, function() log[#log + 1] = 'told' end)
         local function make() require('host').tangle(bridge.table(), other) end
         make()
         collectgarbage()
         collectgarbage()
         return table.concat(log, ' ')",
    );
    assert_eq!(other_told, "", "a script subscriber ran inside collection");
    let reached = reached.lock().unwrap();
    assert_eq!(reached.len(), 3, "the close callback did not run");
    for got in reached.iter() {
        assert!(got.contains(REFUSED), "{got}");
    }
}

#[test]
fn a_close_callback_run_by_dropping_the_runtime_cannot_call_into_lua() {
    let rt = Runtime::new(Profile::Trusted).unwrap();
    let (reached, subscriptions) = tangled(&rt, false);
    eval_in::<()>(
        &rt,
        "other = bridge.table() held = bridge.table() require('host').tangle(held, other)",
    );
    drop(rt);
    drop(subscriptions);
    let reached = reached.lock().unwrap();
    assert_eq!(reached.len(), 3, "the close callback did not run");
    for got in reached.iter() {
        assert!(got.contains(REFUSED) || got.contains("collected"), "{got}");
    }
}

#[test]
fn a_close_callback_panicking_in_collection_or_runtime_drop_is_reported_and_the_rest_run() {
    // Run again as a child of its own, to read what it writes to standard error.
    const CHILD: &str = "AVARICE_BRIDGE_TEST_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let rt = Runtime::new(Profile::Trusted).unwrap();
        let (reached, subscriptions) = tangled(&rt, true);
        eval_in::<()>(
            &rt,
            "other = bridge.table()
             local function make() require('host').tangle(bridge.table(), other) end
             make()
             collectgarbage()
             collectgarbage()
             held = bridge.table()
             require('host').tangle(held, other)",
        );
        assert_eq!(
            reached.lock().unwrap().len(),
            3,
            "collection stopped at the panic"
        );
        drop(rt);
        drop(subscriptions);
        assert_eq!(
            reached.lock().unwrap().len(),
            6,
            "dropping stopped at the panic"
        );
        return;
    }
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "a_close_callback_panicking_in_collection_or_runtime_drop_is_reported_and_the_rest_run",
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
    assert_eq!(
        stderr
            .matches("A bridge.table close callback panicked")
            .count(),
        2,
        "{stderr}"
    );
}

// -- A Bridge cell ----------------------------------------------------------------------------

#[test]
fn bridge_cell_starts_at_nil_or_at_the_value_given_and_set_replaces_it() {
    let got: (bool, i64, String) = eval(
        "local empty, cell = bridge.cell(), bridge.cell(1)
         local before = cell:get()
         cell:set('two')
         return empty:get() == nil, before, cell:get()",
    );
    assert_eq!(got, (true, 1, "two".into()));
}

#[test]
fn update_sets_what_fn_gives_for_the_old_value() {
    let got: (i64, i64) = eval(
        "local cell = bridge.cell(20)
         cell:update(function(old) return old + 1 end)
         local first = cell:get()
         cell:update(function(old) return old * 2, 'ignored' end)
         return first, cell:get()",
    );
    assert_eq!(got, (21, 42));
}

/// Lua that defines `record(old, new)`, which appends `old>new` to `log`, nil shown as `nil`, and
/// `told()`, which gives the log joined with spaces.
const RECORD_CELL: &str = "
    local log = {}
    local function record(old, new) log[#log + 1] = tostring(old) .. '>' .. tostring(new) end
    local function told() return table.concat(log, ' ') end
";

#[test]
fn every_set_tells_a_lua_subscriber_old_and_new_even_of_the_same_value() {
    let got: (String, String) = eval(&format!(
        "{RECORD_CELL}
         local cell = bridge.cell(1)
         local inner = {{}}
         local seen
         cell:subscribe(function(old, new) record(old, new) seen = cell:get() end)
         cell:set(2)
         cell:set(2)
         cell:set(nil)
         cell:set(inner)
         cell:set(inner)
         cell:update(function(old) return 'u' end)
         return told():gsub('table: 0x%x+', 'inner'), seen"
    ));
    assert_eq!(
        got,
        (
            "1>2 2>2 2>nil nil>inner inner>inner inner>u".into(),
            "u".into()
        )
    );
}

#[test]
fn update_whose_fn_raises_raises_it_again_as_it_is_and_changes_nothing() {
    let got: (bool, String, i64, String) = eval(&format!(
        "{RECORD_CELL}
         local cell = bridge.cell(1)
         cell:subscribe(record)
         local raised = {{ code = 7 }}
         local ok, err = pcall(cell.update, cell, function() error(raised) end)
         return rawequal(err, raised), tostring(ok), cell:get(), told()"
    ));
    assert_eq!(got, (true, "false".into(), 1, String::new()));
}

#[test]
fn a_cancel_inside_update_ends_it_as_a_cancel_and_sets_nothing() {
    let cancel = CancelHandle::new();
    let rt = Runtime::builder(Profile::Trusted)
        .cancel_handle(cancel.clone())
        .check_interval(100)
        .build()
        .unwrap();
    let stop = {
        let cancel = cancel.clone();
        rt.lua()
            .create_function(move |_, ()| {
                cancel.cancel();
                Ok(())
            })
            .unwrap()
    };
    rt.lua().globals().set("stop", stop).unwrap();
    let err = rt
        .block_on(rt.eval::<()>(
            "local bridge = require('bridge')
             held = bridge.cell(1)
             told = false
             held:subscribe(function() told = true end)
             held:update(function() stop() while true do end end)",
            "=test",
        ))
        .unwrap_err();
    let Error::Lua(err) = err else {
        panic!("expected a Lua error, got {err:?}");
    };
    assert!(was_cancelled(&err), "{err}");
    let told: bool = rt.lua().globals().get("told").unwrap();
    assert!(!told, "a subscriber was told after the cancel");
    cancel.reset();
    assert_eq!(eval_in::<i64>(&rt, "return held:get()"), 1);
}

/// A trusted runtime with a global `stop()` that cancels it, and the handle that resets it.
fn stoppable() -> (Runtime, CancelHandle) {
    let cancel = CancelHandle::new();
    let rt = Runtime::builder(Profile::Trusted)
        .cancel_handle(cancel.clone())
        .check_interval(100)
        .build()
        .unwrap();
    let stop = {
        let cancel = cancel.clone();
        rt.lua()
            .create_function(move |_, ()| {
                cancel.cancel();
                Ok(())
            })
            .unwrap()
    };
    rt.lua().globals().set("stop", stop).unwrap();
    (rt, cancel)
}

#[test]
fn a_cancel_inside_a_lua_cell_subscriber_ends_the_set_and_tells_no_one_else() {
    let (rt, _cancel) = stoppable();
    let err = rt
        .block_on(rt.eval::<()>(
            "local bridge = require('bridge')
             held = bridge.cell(1)
             later = false
             held:subscribe(function() stop() while true do end end)
             held:subscribe(function() later = true end)
             held:set(2)",
            "=test",
        ))
        .unwrap_err();
    let Error::Lua(err) = err else {
        panic!("expected a Lua error, got {err:?}");
    };
    assert!(was_cancelled(&err), "{err}");
    let later: bool = rt.lua().globals().get("later").unwrap();
    assert!(!later, "a subscriber was told after the cancel");
}

#[test]
fn a_cancel_inside_a_cell_close_callback_ends_the_close_but_still_tells_rust() {
    let (rt, _cancel) = stoppable();
    let told = Told::default();
    let held: Held = Arc::default();
    let watch = {
        let (told, held) = (told.clone(), Arc::clone(&held));
        rt.lua()
            .create_function(move |_, c: bridge::Cell| {
                let told = told.clone();
                let subscription = c.subscribe_with_close(|_, _, _| {}, move || told.closed());
                held.lock().unwrap().push(subscription);
                Ok(())
            })
            .unwrap()
    };
    rt.lua().globals().set("watch", watch).unwrap();
    let err = rt
        .block_on(rt.eval::<()>(
            "local bridge = require('bridge')
             later = false
             held = bridge.cell()
             held:subscribe(print, function() stop() while true do end end)
             watch(held)
             held:subscribe(print, function() later = true end)
             held:close()",
            "=test",
        ))
        .unwrap_err();
    let Error::Lua(err) = err else {
        panic!("expected a Lua error, got {err:?}");
    };
    assert!(was_cancelled(&err), "{err}");
    assert_eq!(told.get(), "closed");
    let later: bool = rt.lua().globals().get("later").unwrap();
    assert!(!later, "a Lua close callback was called after the cancel");
}

#[test]
fn update_whose_fn_closes_the_cell_raises_and_sets_nothing() {
    let got: (String, i64, String) = eval(&format!(
        "{RECORD_CELL}
         local cell = bridge.cell(1)
         cell:subscribe(record, function() record('closed', '') end)
         local ok, err = pcall(cell.update, cell, function(old)
             cell:close()
             return old + 1
         end)
         return tostring(err):match('bridge.cell.update: the Bridge cell is closed') or
             tostring(ok), cell:get(), told()"
    ));
    assert_eq!(
        got,
        (
            "bridge.cell.update: the Bridge cell is closed".into(),
            1,
            "closed>".into()
        )
    );
}

#[test]
fn update_whose_fn_yields_raises_that_it_cannot() {
    let got: (bool, String) = eval(
        "local cell = bridge.cell(1)
         local co = coroutine.wrap(function()
             return pcall(cell.update, cell, function(old) coroutine.yield() return old end)
         end)
         local ok, err = co()
         return ok, tostring(err)",
    );
    assert!(!got.0);
    assert!(
        got.1.contains("bridge.cell.update: fn cannot yield"),
        "{}",
        got.1
    );
}

#[test]
fn a_cell_subscriber_that_sets_has_its_set_told_to_everyone_before_the_one_it_was_told_of() {
    let got: (String, i64) = eval(&format!(
        "{RECORD_CELL}
         local cell = bridge.cell(0)
         cell:subscribe(function(old, new) if new > 10 then cell:set(10) end end)
         cell:subscribe(record)
         cell:set(50)
         return told(), cell:get()"
    ));
    assert_eq!(got, ("50>10 0>50".into(), 10));
}

#[test]
fn a_cell_subscriber_added_or_removed_while_a_set_is_told_hears_only_later_ones() {
    let got: String = eval(&format!(
        "{RECORD_CELL}
         local cell = bridge.cell(0)
         local late
         cell:subscribe(function(old, new)
             if new == 1 then
                 late = cell:subscribe(function(o, n) record('late ' .. o, n) end)
             elseif new == 3 then
                 late:unsubscribe()
             end
         end)
         local handle = cell:subscribe(record)
         cell:set(1)
         cell:set(2)
         handle:unsubscribe()
         cell:set(3)
         cell:set(4)
         return told()"
    ));
    assert_eq!(got, "0>1 1>2 late 1>2");
}

#[test]
fn a_lua_cell_subscriber_that_raises_does_not_fail_the_set_or_starve_the_rest() {
    // Run again as a child of its own, to read what it writes to standard error.
    const CHILD: &str = "AVARICE_BRIDGE_TEST_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let got: (String, i64) = eval(&format!(
            "{RECORD_CELL}
             local cell = bridge.cell()
             cell:subscribe(function() error('the cell subscriber broke') end)
             cell:subscribe(record)
             cell:set(1)
             return told(), cell:get()"
        ));
        assert_eq!(got, ("nil>1".into(), 1));
        return;
    }
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "a_lua_cell_subscriber_that_raises_does_not_fail_the_set_or_starve_the_rest",
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
    assert!(
        stderr.contains("Error running a bridge.cell subscriber")
            && stderr.contains("the cell subscriber broke"),
        "{stderr}"
    );
}

#[test]
fn cell_methods_check_their_arguments() {
    for (call, expected) in [
        (
            "bridge.cell(1, 2)",
            "bridge.cell: expected at most 1 argument, got 2",
        ),
        (
            "bridge.cell().get({})",
            "bridge.cell.get: expected a bridge.cell, got table",
        ),
        (
            "bridge.cell():get(1)",
            "bridge.cell.get: expected no arguments, got 1",
        ),
        (
            "bridge.cell().set(bridge.table(), 1)",
            "bridge.cell.set: expected a bridge.cell, got userdata",
        ),
        (
            "bridge.cell():set(1, 2)",
            "bridge.cell.set: expected at most 1 argument, got 2",
        ),
        (
            "bridge.cell():update('x')",
            "bridge.cell.update: expected a function, got string",
        ),
        (
            "bridge.cell().update(nil, print)",
            "bridge.cell.update: expected a bridge.cell, got nil",
        ),
        (
            "bridge.cell():subscribe('x')",
            "bridge.cell.subscribe: expected a function, got string",
        ),
        (
            "bridge.cell():subscribe(print, 'x')",
            "bridge.cell.subscribe: expected a function for on_close, got string",
        ),
        (
            "bridge.cell():subscribe(print, print, 1)",
            "bridge.cell.subscribe: expected at most 2 arguments, got 3",
        ),
        (
            "bridge.cell().close(true)",
            "bridge.cell.close: expected a bridge.cell, got boolean",
        ),
        (
            "bridge.cell():close(1)",
            "bridge.cell.close: expected no arguments, got 1",
        ),
    ] {
        let message = raises(call);
        assert!(message.contains(expected), "{call}: {message}");
    }
}

#[test]
fn a_bridge_cell_names_itself_and_gives_an_address() {
    let got: (String, String, bool) = eval(
        "local c, d = bridge.cell(), bridge.cell()
         return type(c), tostring(c), tostring(c) ~= tostring(d)",
    );
    assert_eq!(got.0, "userdata");
    assert!(got.1.starts_with("bridge.cell: 0x"), "{}", got.1);
    assert!(got.2, "two Bridge cells have the same address");
}

#[test]
fn closing_a_cell_runs_each_lua_close_callback_once_and_a_second_close_does_nothing() {
    let got: String = eval(
        "local log = {}
         local cell = bridge.cell()
         cell:subscribe(print, function() log[#log + 1] = 'first' end)
         cell:subscribe(print)
         cell:subscribe(print, function() log[#log + 1] = 'second' end)
         cell:close()
         cell:close()
         return table.concat(log, ' ')",
    );
    assert_eq!(got, "first second");
}

#[test]
fn a_closed_cell_can_be_read_and_refuses_set_update_and_subscribe() {
    let got: (i64, String) = eval(
        "local cell = bridge.cell(1)
         local called = false
         cell:subscribe(function() called = true end)
         cell:close()
         local refused = {}
         for _, try in ipairs({
             function() cell:set(2) end,
             function() cell:update(function() called = true return 3 end) end,
             function() cell:subscribe(print) end,
         }) do
             local ok, err = pcall(try)
             local refusal = tostring(err):match('bridge[%w.]+: the Bridge cell is closed')
             refused[#refused + 1] = ok and 'allowed' or refusal
         end
         assert(not called, 'something was called after the close')
         return cell:get(), table.concat(refused, ', ')",
    );
    assert_eq!(
        got,
        (
            1,
            "bridge.cell.set: the Bridge cell is closed, \
             bridge.cell.update: the Bridge cell is closed, \
             bridge.cell.subscribe: the Bridge cell is closed"
                .into()
        )
    );
}

#[test]
fn a_cell_closed_while_a_set_is_told_lets_every_subscriber_hear_it_then_tells_the_close() {
    let got: String = eval(
        "local log = {}
         local cell = bridge.cell(0)
         cell:subscribe(function(old, new)
             log[#log + 1] = 'A:' .. new
             cell:close()
             local ok = pcall(cell.set, cell, 2)
             log[#log + 1] = ok and 'set' or 'refused'
         end, function() log[#log + 1] = 'A closed' end)
         cell:subscribe(function(old, new)
             log[#log + 1] = 'B:' .. new
         end, function() log[#log + 1] = 'B closed' end)
         cell:set(1)
         return table.concat(log, ', ')",
    );
    assert_eq!(got, "A:1, refused, B:1, A closed, B closed");
}

#[test]
fn a_bridge_cell_holding_itself_or_captured_by_its_subscriber_is_collected() {
    let got: bool = eval(
        "local weak = setmetatable({}, { __mode = 'k' })
         local function make()
             local direct = bridge.cell()
             direct:set(direct)
             local captured = bridge.cell()
             local handle
             handle = captured:subscribe(function() return captured, handle end,
                 function() return captured end)
             weak[direct], weak[captured] = true, true
         end
         make()
         assert(next(weak))
         collectgarbage()
         collectgarbage()
         return next(weak) == nil",
    );
    assert!(got, "a Bridge cell in a cycle was kept alive");
}

// -- A Bridge cell from Rust ------------------------------------------------------------------

/// A host module of the test's own, `host`, which receives Bridge cells from the script, as
/// [`Host`] does Bridge tables: `host.watch(c)` subscribes, recording each set into `told` as
/// `old>new`, its close as `closed`; `host.unwatch()` drops every subscription; `host.keep(c)`
/// keeps a handle; `host.set(v)`, `host.get()` and `host.close()` set, get and close through the
/// last one kept or watched.
struct CellHost {
    rt: Runtime,
    told: Told,
    kept: Arc<Mutex<Option<bridge::Cell>>>,
    subscriptions: Arc<Mutex<Vec<bridge::Subscription>>>,
}

impl CellHost {
    fn new() -> Self {
        let host = CellHost {
            rt: Runtime::new(Profile::Trusted).unwrap(),
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
        let watch = lua
            .create_function(move |_, c: bridge::Cell| {
                let (on_set, on_close) = (told.clone(), told.clone());
                let subscription = c.subscribe_with_close(
                    move |_, old, new| on_set.record_set(&old, &new),
                    move || on_close.closed(),
                );
                subscriptions.lock().unwrap().push(subscription);
                *kept.lock().unwrap() = Some(c);
                Ok(())
            })
            .unwrap();
        module.set("watch", watch).unwrap();
        let subscriptions = Arc::clone(&host.subscriptions);
        let unwatch = lua
            .create_function(move |_, ()| {
                subscriptions.lock().unwrap().clear();
                Ok(())
            })
            .unwrap();
        module.set("unwatch", unwatch).unwrap();
        let kept = Arc::clone(&host.kept);
        let keep = lua
            .create_function(move |_, c: bridge::Cell| {
                *kept.lock().unwrap() = Some(c);
                Ok(())
            })
            .unwrap();
        module.set("keep", keep).unwrap();
        let kept = Arc::clone(&host.kept);
        let set = lua
            .create_function(move |_, v: Value| kept.lock().unwrap().clone().unwrap().set(v))
            .unwrap();
        module.set("set", set).unwrap();
        let kept = Arc::clone(&host.kept);
        let get = lua
            .create_function(move |_, ()| kept.lock().unwrap().clone().unwrap().get::<Value>())
            .unwrap();
        module.set("get", get).unwrap();
        let kept = Arc::clone(&host.kept);
        let close = lua
            .create_function(move |_, ()| kept.lock().unwrap().clone().unwrap().close())
            .unwrap();
        module.set("close", close).unwrap();
        host.rt.register_module("host", module).unwrap();
        host
    }

    fn eval<R: mlua::FromLuaMulti>(&self, body: &str) -> R {
        eval_in(&self.rt, &format!("local host = require('host')\n{body}"))
    }

    /// The handle `host` last kept, from outside any Lua call.
    fn kept(&self) -> bridge::Cell {
        self.kept.lock().unwrap().clone().unwrap()
    }
}

#[test]
fn a_host_function_receives_a_bridge_cell_and_is_told_every_set_as_old_and_new() {
    let host = CellHost::new();
    host.eval::<()>(
        "local cell = bridge.cell(1)
         host.watch(cell)
         cell:set(2)
         cell:set(2)
         cell:update(function(old) return old .. '!' end)
         cell:set(nil)",
    );
    assert_eq!(host.told.get(), "1>2 2>2 2>2! 2!>nil");
}

#[test]
fn a_host_function_refuses_what_is_not_a_bridge_cell() {
    let host = CellHost::new();
    let message: String = host.eval(
        "local ok, err = pcall(host.watch, bridge.table())
         assert(not ok)
         return tostring(err)",
    );
    assert!(message.contains("bridge::Cell"), "{message}");
}

#[test]
fn a_set_from_rust_tells_lua_subscribers_inside_that_call_and_rust_ones_too() {
    let host = CellHost::new();
    let got: (String, String, String) = host.eval(&format!(
        "{RECORD_CELL}
         local cell = bridge.cell()
         host.watch(cell)
         cell:subscribe(record)
         host.set('v')
         cell:set('w')
         return told(), cell:get(), host.get()"
    ));
    assert_eq!(got, ("nil>v v>w".into(), "w".into(), "w".into()));
    assert_eq!(host.told.get(), "nil>v v>w");
}

#[test]
fn a_set_from_another_thread_tells_lua_and_rust_subscribers() {
    let host = CellHost::new();
    host.eval::<()>(&format!(
        "{RECORD_CELL}
         log_of = told
         held = bridge.cell(1)
         host.watch(held)
         held:subscribe(record)"
    ));
    let cell = host.kept();
    std::thread::spawn(move || cell.set(3).unwrap())
        .join()
        .unwrap();
    let got: String = host.eval("return log_of()");
    assert_eq!(got, "1>3");
    assert_eq!(host.told.get(), "1>3");
    assert_eq!(host.kept().get::<i64>().unwrap(), 3);
}

#[test]
fn a_rust_cell_subscriber_may_set_the_cell_it_is_told_of() {
    let rt = Runtime::new(Profile::Trusted).unwrap();
    let subscriptions: Arc<Mutex<Vec<bridge::Subscription>>> = Arc::default();
    let clamp = rt
        .lua()
        .create_function(move |_, c: bridge::Cell| {
            let handle = c.clone();
            let subscription = c.subscribe(move |_, _, new| {
                if let Value::Integer(n) = new
                    && n > 10
                {
                    handle.set(10).unwrap();
                }
            });
            subscriptions.lock().unwrap().push(subscription);
            Ok(())
        })
        .unwrap();
    let module = rt.lua().create_table().unwrap();
    module.set("clamp", clamp).unwrap();
    rt.register_module("host", module).unwrap();
    let got: (i64, String) = eval_in(
        &rt,
        &format!(
            "{RECORD_CELL}
             local cell = bridge.cell(0)
             require('host').clamp(cell)
             cell:subscribe(record)
             cell:set(50)
             return cell:get(), told()"
        ),
    );
    assert_eq!(got, (10, "50>10 0>50".into()));
}

#[test]
fn dropping_the_rust_cell_subscription_stops_its_notifications() {
    let host = CellHost::new();
    host.eval::<()>(
        "local cell = bridge.cell()
         host.watch(cell)
         cell:set(1)
         host.unwatch()
         cell:set(2)
         cell:close()",
    );
    assert_eq!(host.told.get(), "nil>1");
}

#[test]
fn closing_a_cell_from_rust_tells_lua_and_rust_in_the_order_they_subscribed() {
    let host = CellHost::new();
    let got: (String, String, i64) = host.eval(
        "local log = {}
         local cell = bridge.cell(1)
         cell:subscribe(print, function() log[#log + 1] = 'lua first' end)
         host.watch(cell)
         cell:subscribe(print, function() log[#log + 1] = 'lua last' end)
         host.close()
         host.close()
         local ok, err = pcall(host.set, 2)
         return table.concat(log, ' '), tostring(err), host.get()",
    );
    assert_eq!(got.0, "lua first lua last");
    assert!(
        got.1.contains("bridge.cell: the Bridge cell is closed"),
        "{}",
        got.1
    );
    assert_eq!(got.2, 1);
    assert_eq!(host.told.get(), "closed");
    let message = host.kept().set(3).unwrap_err().to_string();
    assert!(message.contains("the Bridge cell is closed"), "{message}");
}

#[test]
fn closing_a_cell_from_lua_tells_rust_and_subscribing_after_tells_the_close_at_once() {
    let host = CellHost::new();
    host.eval::<()>(
        "local cell = bridge.cell()
         host.watch(cell)
         cell:close()
         host.unwatch()
         host.watch(cell)",
    );
    assert_eq!(host.told.get(), "closed closed");
}

#[test]
fn collection_closes_a_bridge_cell_the_host_holds_telling_only_rust_close_callbacks() {
    let host = CellHost::new();
    let got: bool = host.eval(
        "lua_told = false
         local weak = setmetatable({}, { __mode = 'k' })
         local function make()
             local cell = bridge.cell(1)
             host.watch(cell)
             cell:subscribe(print, function() lua_told = true end)
             weak[cell] = true
         end
         make()
         collectgarbage()
         collectgarbage()
         assert(next(weak) == nil, 'the host kept the Bridge cell alive')
         return lua_told",
    );
    assert!(!got, "a Lua close callback was called by collection");
    assert_eq!(host.told.get(), "closed");
    let message = host.kept().set(2).unwrap_err().to_string();
    assert!(message.contains("collected"), "{message}");
    assert!(host.kept().get::<Value>().is_err());
    host.kept().close().unwrap();
    assert_eq!(host.told.get(), "closed", "closing it again told it again");
}

#[test]
fn a_cell_close_callback_run_by_collection_cannot_call_into_lua() {
    let rt = Runtime::new(Profile::Trusted).unwrap();
    let reached: Reached = Arc::default();
    let subscriptions: Held = Arc::default();
    let tangle = {
        let (reached, subscriptions) = (Arc::clone(&reached), Arc::clone(&subscriptions));
        rt.lua()
            .create_function(move |_, (c, other): (bridge::Cell, bridge::Cell)| {
                let reached = Arc::clone(&reached);
                let reaching = c.subscribe_with_close(
                    |_, _, _| {},
                    move || {
                        let shown = |e: mlua::Error| e.to_string();
                        let mut reached = reached.lock().unwrap();
                        reached.push(other.set(1).map_err(shown).err().unwrap_or_default());
                        reached.push(
                            other
                                .get::<Value>()
                                .map_err(shown)
                                .err()
                                .unwrap_or_default(),
                        );
                        reached.push(other.close().map_err(shown).err().unwrap_or_default());
                    },
                );
                subscriptions.lock().unwrap().push(reaching);
                Ok(())
            })
            .unwrap()
    };
    let module = rt.lua().create_table().unwrap();
    module.set("tangle", tangle).unwrap();
    rt.register_module("host", module).unwrap();
    let other_told: String = eval_in(
        &rt,
        "local log = {}
         other = bridge.cell()
         other:subscribe(function() log[#log + 1] = 'told' end)
         local function make() require('host').tangle(bridge.cell(), other) end
         make()
         collectgarbage()
         collectgarbage()
         return table.concat(log, ' ')",
    );
    assert_eq!(other_told, "", "a script subscriber ran inside collection");
    let reached = reached.lock().unwrap();
    assert_eq!(reached.len(), 3, "the close callback did not run");
    for got in reached.iter() {
        assert!(
            got.contains("a Bridge cell cannot be used from a close callback run by collection"),
            "{got}"
        );
    }
}

#[test]
fn dropping_the_runtime_with_an_open_bridge_cell_runs_its_rust_close_callbacks() {
    let host = CellHost::new();
    host.eval::<()>(
        "held = bridge.cell(1)
         host.watch(held)
         held:subscribe(print, function() error('not to be called') end)",
    );
    let CellHost {
        rt,
        told,
        subscriptions,
        ..
    } = host;
    drop(rt);
    assert_eq!(told.get(), "closed");
    drop(subscriptions);
}

// -- Seeing what one holds ---------------------------------------------------------------------

#[test]
fn snapshot_gives_a_plain_table_with_the_contents() {
    let got: (String, i64, String, i64, bool) = eval(
        "local inner = {}
         local t = bridge.table({ 10, 20, name = 'x', inner = inner })
         local copy = bridge.table.snapshot(t)
         return type(copy), copy[2], copy.name, #copy, copy.inner == inner",
    );
    assert_eq!(got, ("table".into(), 20, "x".into(), 2, true));
}

#[test]
fn writing_to_a_snapshot_tells_no_one_and_leaves_the_bridge_table_alone() {
    let got: (i64, Option<i64>, i64) = eval(
        "local t = bridge.table({ a = 1 })
         local told = 0
         local _ = bridge.table.subscribe(t, function() told = told + 1 end)
         local copy = bridge.table.snapshot(t)
         copy.a = 2
         copy.b = 3
         return t.a, t.b, told",
    );
    assert_eq!(got, (1, None, 0));
}

#[test]
fn a_snapshot_is_taken_when_asked_and_does_not_follow_later_writes() {
    let got: (i64, i64) = eval(
        "local t = bridge.table({ a = 1 })
         local copy = bridge.table.snapshot(t)
         t.a = 2
         return copy.a, t.a",
    );
    assert_eq!(got, (1, 2));
}

#[test]
fn a_closed_bridge_table_can_still_be_snapshot() {
    let got: i64 = eval(
        "local t = bridge.table({ a = 1 })
         bridge.table.close(t)
         return bridge.table.snapshot(t).a",
    );
    assert_eq!(got, 1);
}

#[test]
fn snapshot_refuses_what_is_not_a_bridge_table() {
    let message = raises("bridge.table.snapshot({})");
    assert!(
        message.contains("bridge.table.snapshot: expected a bridge.table, got table"),
        "{message}"
    );
    let message = raises("bridge.table.snapshot(bridge.cell(1))");
    assert!(
        message.contains("bridge.table.snapshot: expected a bridge.table, got userdata"),
        "{message}"
    );
    let message = raises("bridge.table.snapshot(bridge.table(), 1)");
    assert!(
        message.contains("bridge.table.snapshot: expected at most 1 argument, got 2"),
        "{message}"
    );
}

#[cfg(feature = "stdlib-serde")]
#[test]
fn json_encodes_a_bridge_table_as_its_contents() {
    let got: (String, String) = eval(
        "local json = require('serde').json
         return json.encode(bridge.table({ 1, 2, 3 })), json.encode(bridge.table({ a = 'x' }))",
    );
    assert_eq!(got, ("[1,2,3]".into(), r#"{"a":"x"}"#.into()));
}

#[cfg(feature = "stdlib-serde")]
#[test]
fn json_encodes_nested_bridge_tables_all_the_way_down() {
    let got: String = eval(
        "local json = require('serde').json
         local t = bridge.table({
             list = bridge.table({ 1, bridge.table({ deep = true }) }),
             plain = { cell = bridge.cell('c') },
         })
         local back = json.decode(json.encode(t))
         return table.concat({
             tostring(back.list[1]), tostring(back.list[2].deep), back.plain.cell,
         }, ',')",
    );
    assert_eq!(got, "1,true,c");
}

#[cfg(feature = "stdlib-serde")]
#[test]
fn json_encodes_a_bridge_cell_as_its_value() {
    let got: (String, String, String) = eval(
        "local json = require('serde').json
         return json.encode(bridge.cell(42)), json.encode(bridge.cell({ 'a', 'b' })),
             json.encode(bridge.cell(bridge.table({ n = 1 })))",
    );
    assert_eq!(
        got,
        ("42".into(), r#"["a","b"]"#.into(), r#"{"n":1}"#.into())
    );
}

#[cfg(feature = "stdlib-serde")]
#[test]
fn json_encodes_a_closed_bridge_table_as_what_it_held() {
    let got: String = eval(
        "local json = require('serde').json
         local t = bridge.table({ a = 1 })
         bridge.table.close(t)
         return json.encode(t)",
    );
    assert_eq!(got, r#"{"a":1}"#);
}

#[cfg(feature = "stdlib-serde")]
#[test]
fn json_encoding_a_bridge_table_that_contains_itself_raises() {
    for (what, body) in [
        ("directly", "local t = bridge.table() t.me = t return t"),
        (
            "through another",
            "local a, b = bridge.table(), bridge.table() a.b = b b.a = a return a",
        ),
        (
            "through a plain table",
            "local t = bridge.table() t.plain = { back = t } return t",
        ),
        (
            "through a cell",
            "local t = bridge.table() t.cell = bridge.cell(t) return t",
        ),
    ] {
        let message = raises(&format!(
            "local json = require('serde').json
             local t = (function() {body} end)()
             json.encode(t)"
        ));
        assert!(message.contains("contains itself"), "{what}: {message}");
    }
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
