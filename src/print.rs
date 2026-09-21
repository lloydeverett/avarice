//! `print`, and the write sink it writes to.
//!
//! `print` itself is Lua (`print.lua`; the reasons are in ADR 0005). What is Rust is the one
//! function it is given, which appends bytes to the runtime's write sink.

use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use mlua::chunk::ChunkMode;
use mlua::{Function, Lua, LuaString};

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

fn flush_c_stdio() {
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

/// Replaces `print` with the one that writes to `sink`.
///
/// Runs before the runtime's limits are installed, like the rest of setup.
pub(crate) fn install(lua: &Lua, sink: Sink) -> mlua::Result<()> {
    let write = lua.create_function(move |_, text: LuaString| {
        let mut sink = lock(&sink);
        sink.write_all(&text.as_bytes())
            .and_then(|()| sink.flush())
            .map_err(|e| mlua::Error::runtime(format!("could not write output: {e}")))
    })?;
    let print: Function = lua
        .load(PRINT)
        .set_name("=[avarice-rt print]")
        .set_mode(ChunkMode::Text)
        .call(write)?;
    lua.globals().raw_set("print", print)
}
