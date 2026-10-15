//! `bridge` is original to avarice, not derived from Astra: ADR 0022 records why. It gives Lua two
//! values the host can watch: a **Bridge table**, which a script writes and reads as a Lua table,
//! and a **Bridge cell**, which holds one value, replaced whole. What is each one's alone is in
//! `table.rs` and `cell.rs`; what any Bridge object has, its subscribers, the telling of a change,
//! its closing and the host's weak hold, is in `shared.rs`.
//!
//! Where each part lives is decided by the garbage collector. ADR 0022 has Lua own a Bridge
//! object, collected once Lua holds no reference to it, and a script will store one in itself, or
//! in a closure stored in itself. So whatever is Lua's lives in Lua, as the userdata's user
//! values, which the collector traces: its contents, and Lua's subscribers when there are any. A
//! Lua value held from Rust is a registry reference, which the collector treats as a root, so a
//! Bridge object holding itself through one would never be collected. Rust holds only what is not
//! Lua's, with the Rust subscribers. The host's handle on one is likewise weak: an id it looks up
//! in a weak-valued table, through a `WeakLua`, and never an `AnyUserData` or an `mlua::Table`,
//! which are registry references too.
//!
//! A subscriber is called inside the write, under the Lua lock, even when the host writes from
//! another thread, and with no lock of this module's held, since it may write in turn or subscribe
//! or unsubscribe. Nor is a subscriber dropped under one, since it may own a subscription whose
//! drop takes it.
//!
//! A Bridge object ends by being closed: from Lua, from the host, or by the userdata's `Drop`,
//! when it is collected or the runtime is dropped. Each subscriber is taken out in turn and told by
//! its close callback, outside any lock, so a close callback may drop a subscription. `Drop` has
//! no `Lua`, and Lua may be tearing down, so it tells only the Rust close callbacks, which take
//! none; a Lua one is collected with the object. `Drop` runs inside Lua's collector, with no
//! limits, so while it runs the host's handles refuse to call into Lua, and a close callback's
//! panic is caught there. A close while a change is being told refuses writes at once and tells
//! its close callbacks once the outermost telling ends, so that the close is the last a subscriber
//! hears. The flags are in Rust, so that the host's handles find them without calling into Lua.
//!
//! Nothing here sets a global. [`rust_half`] builds the module's table, and `modules.rs` hands it
//! to `lua/bridge.lua`, which returns it.

mod cell;
mod shared;
mod table;

use mlua::{Function, IntoLua, IntoLuaMulti, Lua, Value};

pub use cell::Cell;
pub use shared::Subscription;
pub use table::{Change, Table};

use shared::Result;

/// Builds the module's table: `bridge.table`, a table whose `__call` makes a Bridge table and
/// whose fields are the operations on one, and `bridge.cell`, which makes a Bridge cell.
pub fn rust_half(lua: &Lua) -> Result<Value> {
    shared::register(lua)?;
    let helpers = Helpers::load(lua)?;
    let module = lua.create_table()?;
    module.set("table", table::build(lua, &helpers)?)?;
    module.set("cell", cell::build(lua, &helpers)?)?;
    Ok(Value::Table(module))
}

/// The parts of the module that are Lua, since Rust can neither raise a Lua value as it is nor
/// read the value a function raised without making it a string. They are built from Lua as it was
/// when the module was built, so that a script that later replaces `xpcall` or `error` does not
/// change them.
#[derive(Clone)]
struct Helpers {
    /// `protect(what, fn, ...)` calls `fn(...)` and gives back whether it returned, whether a
    /// handler saw it raise (Lua calls none for a memory error), and its first result or what it
    /// raised. A yield cannot reach past the Rust that calls it, and that is said plainly, as of
    /// `what`.
    protect: Function,
    /// `raising(run)` makes a function that calls `run`, which gives back whether it succeeded and
    /// what was raised if not, and raises that again as it is.
    raising: Function,
}

const HELPERS: &str = "
    local xpcall, error = xpcall, error
    local cannot_yield = {
        ['attempt to yield across a C-call boundary'] = true,
        ['attempt to yield from outside a coroutine'] = true,
    }
    local function protect(what, fn, ...)
        local handled = false
        local function handler(raised)
            handled = true
            if cannot_yield[raised] then return what .. ': fn cannot yield' end
            return raised
        end
        local function settle(ok, raised) return ok, handled, raised end
        return settle(xpcall(fn, handler, ...))
    end
    local function raising(run)
        return function(...)
            local ok, raised = run(...)
            if not ok then error(raised, 0) end
        end
    end
    return protect, raising
";

impl Helpers {
    fn load(lua: &Lua) -> Result<Self> {
        let (protect, raising) = lua.load(HELPERS).set_name("=[bridge]").call(())?;
        Ok(Helpers { protect, raising })
    }

    /// Calls `body` with `args` under `protect`, as of `what`, giving back its first result, or
    /// what it raised, to be raised again through a function `raising` made. A limit, or memory
    /// running out, is not something `body` raised but the run ending, and is an error.
    fn protect(
        &self,
        lua: &Lua,
        what: &str,
        body: Function,
        args: impl IntoLuaMulti,
    ) -> Result<std::result::Result<Value, Value>> {
        let mut call = args.into_lua_multi(lua)?;
        call.push_front(Value::Function(body));
        call.push_front(what.into_lua(lua)?);
        let (ok, handled, raised): (bool, bool, Value) = self.protect.call(call)?;
        match raised {
            raised if ok => Ok(Ok(raised)),
            // Lua calls no handler for a memory error, and gives only its message.
            raised if !handled => Err(mlua::Error::MemoryError(raised.to_string()?)),
            Value::Error(raised) if shared::is_limit(&raised) => Err(*raised),
            raised => Ok(Err(raised)),
        }
    }
}
