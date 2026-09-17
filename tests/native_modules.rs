//! Registering a module through a `luaopen_*` C entry point.
//!
//! The entry points here are written in Rust rather than C, because what is under test is the
//! registration path, not the C compiler. A real native module differs only in which language
//! produced the same `lua_CFunction`.
//!
//! Every `register_native_module` call below satisfies the same safety contract: the entry
//! point it is handed is one of the two defined in this file, compiled in this test binary
//! against `avarice_rt::mlua::ffi` — the very Lua the crate vendors — so the version skew the
//! contract warns about cannot arise. Each call repeats that as a one-line safety comment.

use std::ffi::c_int;

use avarice_rt::mlua::{ffi, lua_State};
use avarice_rt::{Profile, Runtime};

/// A well-behaved entry point: returns a table, and uses the name Lua passed it.
///
/// This is the shape `luaL_requiref` expects, and the shape every Lua C module has.
unsafe extern "C-unwind" fn luaopen_demo(state: *mut lua_State) -> c_int {
    // Safety: Lua calls an entry point with a live state and the module name at index 1, and
    // guarantees stack space for the arguments it passed. Pushing three values needs no more.
    unsafe {
        ffi::lua_createtable(state, 0, 2);
        // Argument 1 is the module name, exactly as Lua's own module loading passes it.
        ffi::lua_pushvalue(state, 1);
        ffi::lua_setfield(state, -2, c"name".as_ptr());
        ffi::lua_pushinteger(state, 42);
        ffi::lua_setfield(state, -2, c"answer".as_ptr());
    }
    1
}

/// An entry point that returns a single value rather than a table.
unsafe extern "C-unwind" fn luaopen_scalar(state: *mut lua_State) -> c_int {
    // Safety: as above — a live state, and room for the one value pushed.
    unsafe {
        ffi::lua_pushinteger(state, 7);
    }
    1
}

#[test]
fn a_native_module_is_registered_and_required_like_any_other() {
    let rt = Runtime::new(Profile::Trusted).unwrap();
    // Safety: an entry point defined above, against this crate's own Lua.
    unsafe { rt.register_native_module("demo", luaopen_demo) }.unwrap();

    assert!(rt.has_module("demo"));
    let answer: i64 = rt.eval("return require('demo').answer", "=test").unwrap();
    assert_eq!(answer, 42);
}

#[test]
fn the_entry_point_is_told_the_module_name() {
    let rt = Runtime::new(Profile::Trusted).unwrap();
    // Safety: an entry point defined above, against this crate's own Lua.
    unsafe { rt.register_native_module("named.thing", luaopen_demo) }.unwrap();
    let name: String = rt
        .eval("return require('named.thing').name", "=test")
        .unwrap();
    assert_eq!(name, "named.thing");
}

#[test]
fn a_native_module_need_not_return_a_table() {
    let rt = Runtime::new(Profile::Trusted).unwrap();
    // Safety: an entry point defined above, against this crate's own Lua.
    unsafe { rt.register_native_module("scalar", luaopen_scalar) }.unwrap();
    let value: i64 = rt.eval("return require('scalar')", "=test").unwrap();
    assert_eq!(value, 7);
}

#[test]
fn it_runs_once_at_registration_rather_than_on_each_require() {
    let rt = Runtime::new(Profile::Trusted).unwrap();
    // Safety: an entry point defined above, against this crate's own Lua.
    unsafe { rt.register_native_module("demo", luaopen_demo) }.unwrap();
    let same: bool = rt
        .eval("return require('demo') == require('demo')", "=test")
        .unwrap();
    assert!(same);
}

#[test]
fn the_sandbox_can_be_given_a_native_module_too() {
    // The profile decides which runtimes get it; the sandbox does not forbid one outright.
    let rt = Runtime::new(Profile::Sandbox).unwrap();
    // Safety: an entry point defined above, against this crate's own Lua.
    unsafe { rt.register_native_module("demo", luaopen_demo) }.unwrap();
    let answer: i64 = rt.eval("return require('demo').answer", "=test").unwrap();
    assert_eq!(answer, 42);

    // And a sandbox built without it does not have it, which is the point of doing this per
    // runtime rather than globally.
    let plain = Runtime::new(Profile::Sandbox).unwrap();
    assert!(!plain.has_module("demo"));
    assert!(plain.exec("require('demo')", "=test").is_err());
}

#[test]
fn an_ill_formed_name_is_refused_before_the_entry_point_runs() {
    let rt = Runtime::new(Profile::Trusted).unwrap();
    // Safety: an entry point defined above, against this crate's own Lua.
    let err = unsafe { rt.register_native_module("../evil", luaopen_demo) }.unwrap_err();
    assert!(matches!(err, avarice_rt::Error::ModuleName(_)), "{err:?}");
}

#[test]
fn lua_still_cannot_load_native_code_itself() {
    let rt = Runtime::new(Profile::Trusted).unwrap();
    // Safety: an entry point defined above, against this crate's own Lua.
    unsafe { rt.register_native_module("demo", luaopen_demo) }.unwrap();
    // Registering a native module does not open `package`, so there is still no `loadlib`.
    let closed: bool = rt
        .eval("return package == nil and loadlib == nil", "=test")
        .unwrap();
    assert!(closed);
}
