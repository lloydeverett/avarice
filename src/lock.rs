//! One way to lock a mutex, shared by everything here that holds one.

use std::sync::{Mutex, MutexGuard, PoisonError};

/// Locks a mutex, carrying on if a panic elsewhere poisoned it. Nothing under these locks is
/// held across a call into Lua or a loader, so a panic cannot leave one half-updated.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
