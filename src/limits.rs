//! Cancellation and wall-clock limits, enforced from a global debug hook.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use mlua::{HookTriggers, Lua, VmState};

use crate::error::{Cancelled, TimedOut};

/// How many VM instructions pass between limit checks, unless configured otherwise.
///
/// Small enough that a cancel is noticed promptly, large enough that the check does not show up
/// in a profile.
pub const DEFAULT_CHECK_INTERVAL: u32 = 10_000;

/// A thread-safe handle for stopping a runtime that is already executing.
///
/// A handle cloned out of the [`Runtime`](crate::Runtime) can be parked in a signal handler or
/// handed to a watchdog thread, and cancelling through it never has to wait for the runtime.
///
/// Cancellation latches. Once tripped, every subsequent limit check fails too, so a `pcall` in
/// the script cannot swallow the error and carry on; the handle must be [`reset`](Self::reset)
/// before the runtime will run anything again.
#[derive(Debug, Clone, Default)]
pub struct CancelHandle {
    flag: Arc<AtomicBool>,
}

impl CancelHandle {
    /// A handle that has not been tripped.
    pub fn new() -> Self {
        CancelHandle::default()
    }

    /// Asks the runtime to stop at its next limit check.
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::Relaxed);
    }

    /// Whether this handle has been tripped.
    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::Relaxed)
    }

    /// Clears the handle, so the runtime will execute again.
    pub fn reset(&self) {
        self.flag.store(false, Ordering::Relaxed);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Trip {
    Cancelled,
    TimedOut,
}

impl Trip {
    fn to_error(self) -> mlua::Error {
        match self {
            Trip::Cancelled => mlua::Error::external(Cancelled),
            Trip::TimedOut => mlua::Error::external(TimedOut),
        }
    }
}

/// A `std::cell::Cell` that is `Sync`.
///
/// mlua's `send` feature, which the stdlib crate needs for its tasks, makes the hook closure
/// `Send`, so what it shares with the runtime has to be `Sync`.
#[derive(Debug, Default)]
struct SyncCell<T>(Mutex<T>);

impl<T: Copy> SyncCell<T> {
    fn get(&self) -> T {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn set(&self, value: T) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = value;
    }
}

/// The state the hook consults, shared between the hook closure and the runtime.
#[derive(Debug, Default)]
pub(crate) struct Limits {
    cancel: Option<CancelHandle>,
    time_limit: Option<Duration>,
    deadline: SyncCell<Option<Instant>>,
    depth: SyncCell<u32>,
    tripped: SyncCell<Option<Trip>>,
}

impl Limits {
    pub(crate) fn new(cancel: Option<CancelHandle>, time_limit: Option<Duration>) -> Self {
        Limits {
            cancel,
            time_limit,
            ..Limits::default()
        }
    }

    /// Whether anything here needs a hook installed to enforce it.
    pub(crate) fn needs_hook(&self) -> bool {
        self.cancel.is_some() || self.time_limit.is_some()
    }

    pub(crate) fn cancel_handle(&self) -> Option<&CancelHandle> {
        self.cancel.as_ref()
    }

    pub(crate) fn time_limit(&self) -> Option<Duration> {
        self.time_limit
    }

    /// Starts a top-level execution: arms the clock and clears the latch.
    ///
    /// Nested calls only bump the depth, so a host function that re-enters the runtime does not
    /// hand the script a fresh time budget.
    fn enter(&self) {
        let depth = self.depth.get();
        if depth == 0 {
            self.tripped.set(None);
            self.deadline
                .set(self.time_limit.map(|limit| Instant::now() + limit));
        }
        self.depth.set(depth + 1);
    }

    fn leave(&self) {
        let depth = self.depth.get().saturating_sub(1);
        self.depth.set(depth);
        if depth == 0 {
            self.deadline.set(None);
        }
    }

    /// Checked before an execution starts, so that a cancel is honoured even by a chunk too
    /// short to reach a single hook tick.
    pub(crate) fn precheck(&self) -> Option<mlua::Error> {
        let cancelled = self.cancel.as_ref().is_some_and(CancelHandle::is_cancelled);
        if cancelled {
            self.tripped.set(Some(Trip::Cancelled));
            return Some(Trip::Cancelled.to_error());
        }
        None
    }

    /// What the hook runs. Returns the error to raise, if any.
    fn check(&self) -> Option<mlua::Error> {
        if let Some(trip) = self.tripped.get() {
            return Some(trip.to_error());
        }
        if let Some(cancel) = &self.cancel {
            if cancel.is_cancelled() {
                self.tripped.set(Some(Trip::Cancelled));
                return Some(Trip::Cancelled.to_error());
            }
        }
        if let Some(deadline) = self.deadline.get() {
            if Instant::now() >= deadline {
                self.tripped.set(Some(Trip::TimedOut));
                return Some(Trip::TimedOut.to_error());
            }
        }
        None
    }
}

/// Installs the limit hook.
///
/// Deliberately [`Lua::set_global_hook`] rather than [`Lua::set_hook`]: mlua keys a `set_hook`
/// callback to the thread that set it, so a script could shed the limit simply by running its
/// work inside a coroutine.
///
/// The hook triggers on function calls as well as on an instruction count, which is what stops
/// `while true do pcall(spin) end` from running forever. The instruction hook nearly always
/// lands *inside* the protected call, where `pcall` catches it; the call hook fires as `pcall`
/// itself is entered, before it has established its protection, so the latched error propagates
/// out of the loop instead of being swallowed again.
pub(crate) fn install_hook(lua: &Lua, limits: Arc<Limits>, interval: u32) -> mlua::Result<()> {
    let triggers = HookTriggers::new()
        .every_nth_instruction(interval.max(1))
        .on_calls();
    lua.set_global_hook(triggers, move |_, _| match limits.check() {
        Some(err) => Err(err),
        None => Ok(VmState::Continue),
    })
}

/// A top-level execution in progress. Arms the time limit for as long as it is held.
///
/// Returned by [`Runtime::enter`](crate::Runtime::enter) for embedders driving Lua through
/// [`Runtime::lua`](crate::Runtime::lua) directly; the runtime's own `exec` and `eval` take one
/// for themselves.
#[must_use = "limits apply only while the Execution guard is held"]
pub struct Execution {
    limits: Arc<Limits>,
}

impl Execution {
    pub(crate) fn new(limits: Arc<Limits>) -> Self {
        limits.enter();
        Execution { limits }
    }
}

impl Drop for Execution {
    fn drop(&mut self) {
        self.limits.leave();
    }
}

impl std::fmt::Debug for Execution {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Execution").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancel_latches_until_reset() {
        let cancel = CancelHandle::new();
        let limits = Limits::new(Some(cancel.clone()), None);

        limits.enter();
        assert!(limits.check().is_none());
        cancel.cancel();
        assert!(limits.check().is_some());
        // Even once the handle is cleared, the latch keeps failing this execution.
        cancel.reset();
        assert!(limits.check().is_some());
        // A fresh top-level execution clears it.
        limits.leave();
        limits.enter();
        assert!(limits.check().is_none());
    }

    #[test]
    fn nested_entries_keep_the_outer_deadline() {
        let limits = Limits::new(None, Some(Duration::from_secs(60)));
        limits.enter();
        let outer = limits.deadline.get().unwrap();
        limits.enter();
        assert_eq!(limits.deadline.get().unwrap(), outer);
        limits.leave();
        // Still armed: the outer execution has not finished.
        assert_eq!(limits.deadline.get().unwrap(), outer);
        limits.leave();
        assert!(limits.deadline.get().is_none());
    }

    #[test]
    fn deadline_in_the_past_trips_immediately() {
        let limits = Limits::new(None, Some(Duration::ZERO));
        limits.enter();
        assert!(limits.check().is_some());
    }

    #[test]
    fn no_limits_means_no_hook() {
        assert!(!Limits::new(None, None).needs_hook());
        assert!(Limits::new(Some(CancelHandle::new()), None).needs_hook());
        assert!(Limits::new(None, Some(Duration::from_secs(1))).needs_hook());
    }
}
