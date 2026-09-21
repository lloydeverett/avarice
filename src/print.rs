//! `print`, and the write sink it writes to.
//!
//! `print` itself is Lua (`print.lua`; the reasons are in ADR 0005). What is Rust is a pair of
//! functions it is given: one appends bytes to the runtime's write sink, and the other says what
//! parameters a function takes, which Lua can only be asked through its C API. It is also given
//! the `ansi` table it highlights with (ADR 0011).

use std::ffi::{CStr, c_int};
use std::io::{self, Write};
use std::ptr;
use std::sync::{Arc, Mutex};

use mlua::chunk::ChunkMode;
use mlua::{Function, Lua, LuaString, ffi};

use crate::ansi;
use crate::lock::lock;

const PRINT: &str = include_str!("print.lua");

/// The runtime's write sink, shared between the runtime and the function `print` holds.
pub(crate) type Sink = Arc<Mutex<Box<dyn Write + Send>>>;

pub(crate) fn new_sink(writer: impl Write + Send + 'static) -> Sink {
    Arc::new(Mutex::new(Box::new(writer)))
}

/// The sink a runtime has until told otherwise: the process's standard output.
pub(crate) fn default_sink() -> Sink {
    new_sink(Stdout)
}

/// Standard output, written so that it interleaves with Lua's own `io.write`.
///
/// `io.write` goes through C stdio's buffer, which Rust's `stdout` does not share, so on a pipe
/// `io.write("a") print("b")` would come out as `b` then `a`. Flushing C's buffers first puts
/// what `io.write` has written ahead of what is about to be.
struct Stdout;

impl Write for Stdout {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        flush_c_stdio();
        io::stdout().write(bytes)
    }

    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        flush_c_stdio();
        io::stdout().write_all(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        io::stdout().flush()
    }
}

/// Flushes the buffers of C stdio, which is where Lua's `io.write` puts its output.
///
/// A write sink that ends up at the process's standard output should call this before it writes,
/// so that what `io.write` has written comes out ahead of what is about to be: Rust's `stdout`
/// does not share C's buffer, and on a pipe the two would otherwise come out in the wrong order.
/// The default sink does. An embedder that replaces it with something that wraps `stdout` needs
/// to do the same for `print` and `io.write` to interleave.
pub fn flush_c_stdio() {
    unsafe extern "C" {
        fn fflush(stream: *mut std::ffi::c_void) -> std::ffi::c_int;
    }
    // SAFETY: `fflush` with a null stream flushes every open output stream and touches nothing
    // else. The C library is already linked, because Lua is C. Its result is ignored: a failed
    // flush of `io`'s buffer is `io`'s to report, not ours.
    unsafe {
        fflush(std::ptr::null_mut());
    }
}

/// The parameters of a function written in Lua, spelled as its definition spells them:
/// `a, b, ...`.
/// `None` for a function not written in Lua, such as one of Lua's own library functions or a
/// method on a Rust userdata, whose parameters nothing knows.
///
/// A parameter has no name to give if the function's debug information was stripped, and is
/// written `?` then. Lua has no types to report, and cannot say that a parameter is optional.
fn parameter_list(lua: &Lua, function: &Function) -> mlua::Result<Option<String>> {
    let info = function.info();
    if info.what == "C" {
        return Ok(None);
    }
    let mut names = Vec::with_capacity(usize::from(info.num_params) + 1);
    // SAFETY: the closure runs in a protected call with `function` as the one value on the stack,
    // and pops that one value before it returns, so nothing is left for `exec_raw` to collect.
    // `lua_getlocal` with no activation record and a function on top reads that function's
    // parameter names: it pushes nothing, cannot raise an error, and returns either null or a
    // string that lives as long as the function does, so it is copied before the function is
    // popped.
    unsafe {
        lua.exec_raw::<()>(function.clone(), |state| {
            for n in 1..=c_int::from(info.num_params) {
                let name = ffi::lua_getlocal(state, ptr::null(), n);
                names.push(if name.is_null() {
                    "?".into()
                } else {
                    CStr::from_ptr(name).to_string_lossy().into_owned()
                });
            }
            ffi::lua_pop(state, 1);
        })?;
    }
    if info.is_vararg {
        names.push("...".into());
    }
    Ok(Some(names.join(", ")))
}

/// Replaces `print` with the one that writes to `sink`.
///
/// Runs before the runtime's limits are installed, like the rest of setup, and needs the `string`
/// and `table` libraries, which [`RuntimeBuilder::build`](crate::RuntimeBuilder::build) has checked
/// are there.
pub(crate) fn install(lua: &Lua, sink: Sink) -> mlua::Result<()> {
    let write = lua.create_function(move |_, text: LuaString| {
        let mut sink = lock(&sink);
        sink.write_all(&text.as_bytes())
            .and_then(|()| sink.flush())
            .map_err(|e| mlua::Error::runtime(format!("could not write output: {e}")))
    })?;
    let parameters =
        lua.create_function(|lua, function: Function| parameter_list(lua, &function))?;
    let print: Function = lua
        .load(PRINT)
        .set_name("=[avarice-rt print]")
        .set_mode(ChunkMode::Text)
        .call((write, parameters, ansi::build(lua)?))?;
    lua.globals().raw_set("print", print)
}
