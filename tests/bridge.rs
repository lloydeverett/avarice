//! The `bridge` module (ADR 0022), driven the way a script does: Lua through a trusted runtime.
//!
//! A Bridge table is written and read as a Lua table is, so most of these run the same Lua on a
//! Bridge table that a plain table would answer, and check the answer a plain table gives.

#![cfg(feature = "stdlib-bridge")]

mod common;

use avarice::{Profile, Runtime, StdModules};

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
