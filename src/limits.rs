//! Cancellation and wall-clock limits, enforced from a global debug hook.

use std::future::{poll_fn, Future};
use std::pin::pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::{Duration, Instant};

use mlua::{HookTriggers, Lua, VmState};
use tokio::sync::Notify;

use crate::error::{Cancelled, TimedOut};
use crate::lock::lock;

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
/// Cancelling stops the runtime whichever way it is busy. Lua that is running hits the limit
/// check at its next tick; a chunk that is awaiting something, with no Lua running to notice, is
/// woken and stopped at once. Either way the chunk fails with [`Cancelled`].
///
/// Cancellation latches. Once tripped, every subsequent limit check fails too, so a `pcall` in
/// the script cannot swallow the error and carry on; the handle must be [`reset`](Self::reset)
/// before the runtime will run anything again.
#[derive(Debug, Clone, Default)]
pub struct CancelHandle {
    inner: Arc<CancelState>,
}

#[derive(Debug, Default)]
struct CancelState {
    flag: AtomicBool,
    /// Wakes whatever is waiting in [`CancelHandle::cancelled`].
    tripped: Notify,
}

impl CancelHandle {
    /// A handle that has not been tripped.
    pub fn new() -> Self {
        CancelHandle::default()
    }

    /// Asks the runtime to stop: at its next limit check if Lua is running, immediately if the
    /// runtime is waiting on something.
    pub fn cancel(&self) {
        self.inner.flag.store(true, Ordering::Relaxed);
        self.inner.tripped.notify_waiters();
    }

    /// Whether this handle has been tripped.
    pub fn is_cancelled(&self) -> bool {
        self.inner.flag.load(Ordering::Relaxed)
    }

    /// Clears the handle, so the runtime will execute again.
    pub fn reset(&self) {
        self.inner.flag.store(false, Ordering::Relaxed);
    }

    /// Completes once the handle is tripped, or at once if it already is.
    ///
    /// For an embedder that drives Lua through [`Runtime::lua`](crate::Runtime::lua) and wants
    /// to give way to a cancel while awaiting; [`Runtime::run`](crate::Runtime::run) does this
    /// for itself.
    pub async fn cancelled(&self) {
        let mut notified = pin!(self.inner.tripped.notified());
        loop {
            // Registered before the flag is read, so that a cancel between the two is not lost.
            notified.as_mut().enable();
            if self.is_cancelled() {
                return;
            }
            notified.as_mut().await;
            notified.set(self.inner.tripped.notified());
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Trip {
    Cancelled,
    TimedOut,
}

impl Trip {
    pub(crate) fn to_error(self) -> mlua::Error {
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
        *lock(&self.0)
    }

    fn set(&self, value: T) {
        *lock(&self.0) = value;
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
    ///
    /// Also what a wait that is not running Lua asks of itself, so that a time limit or a cancel
    /// ends it just as it would end a script.
    pub(crate) fn check(&self) -> Option<mlua::Error> {
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

/// Runs `future`, unless `cancel` trips while it is pending, in which case it is dropped and the
/// answer is `None`.
///
/// `future` is polled first, so one that is ready is not thrown away for a cancel that arrived at
/// the same moment.
pub(crate) async fn unless_cancelled<T>(
    cancel: Option<&CancelHandle>,
    future: impl Future<Output = T>,
) -> Option<T> {
    let Some(cancel) = cancel else {
        return Some(future.await);
    };
    let mut future = pin!(future);
    let mut cancelled = pin!(cancel.cancelled());
    poll_fn(|cx| {
        if let Poll::Ready(value) = future.as_mut().poll(cx) {
            return Poll::Ready(Some(value));
        }
        match cancelled.as_mut().poll(cx) {
            Poll::Ready(()) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    })
    .await
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

    /// Runs a future to completion on a throwaway executor, giving up after a while so that a
    /// wake that never comes fails the test rather than hanging it.
    fn block_on<T>(future: impl Future<Output = T>) -> T {
        let executor = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        executor
            .block_on(async { tokio::time::timeout(Duration::from_secs(10), future).await })
            .expect("the future was never woken")
    }

    #[test]
    fn cancelled_is_ready_at_once_for_a_handle_already_tripped() {
        let cancel = CancelHandle::new();
        cancel.cancel();
        block_on(cancel.cancelled());
    }

    #[test]
    fn cancelled_is_woken_by_a_cancel_from_another_thread() {
        let cancel = CancelHandle::new();
        let canceller = {
            let cancel = cancel.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(50));
                cancel.cancel();
            })
        };
        block_on(cancel.cancelled());
        canceller.join().unwrap();
    }

    #[test]
    fn a_reset_handle_waits_again() {
        let cancel = CancelHandle::new();
        cancel.cancel();
        cancel.reset();
        let waited = block_on(unless_cancelled(None, async {
            // Not ready yet, so this is a real wait rather than a lucky first poll.
            tokio::time::timeout(Duration::from_millis(50), cancel.cancelled()).await
        }))
        .unwrap();
        assert!(waited.is_err(), "a reset handle should not read as tripped");
    }

    #[test]
    fn unless_cancelled_drops_a_pending_future_on_cancel() {
        let cancel = CancelHandle::new();
        let canceller = {
            let cancel = cancel.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(50));
                cancel.cancel();
            })
        };
        let outcome = block_on(unless_cancelled(
            Some(&cancel),
            std::future::pending::<()>(),
        ));
        canceller.join().unwrap();
        assert!(outcome.is_none());
    }

    #[test]
    fn unless_cancelled_prefers_a_future_that_is_ready() {
        let cancel = CancelHandle::new();
        cancel.cancel();
        assert_eq!(
            block_on(unless_cancelled(Some(&cancel), async { 7 })),
            Some(7)
        );
        assert_eq!(block_on(unless_cancelled(None, async { 7 })), Some(7));
    }

    #[test]
    fn no_limits_means_no_hook() {
        assert!(!Limits::new(None, None).needs_hook());
        assert!(Limits::new(Some(CancelHandle::new()), None).needs_hook());
        assert!(Limits::new(None, Some(Duration::from_secs(1))).needs_hook());
    }
}
