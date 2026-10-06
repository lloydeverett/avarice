//! Ctrl-C while `avarice` is running something.

use std::future::Future;
use std::io;
use std::task::{Context, Poll, Waker};

use avarice::CancelHandle;

/// Makes Ctrl-C trip `cancel`, from now until the process ends.
///
/// The listener cannot be a task on the runtime's own executor, because that executor is what is
/// busy: a Lua loop that never awaits never gives it a turn, so the signal would sit unread. It
/// lives on a thread of its own, whose whole job is to trip the handle. The handle does the rest.
/// Lua that is running stops at its next limit check, and a chunk that is awaiting is woken and
/// dropped, so both are covered without this file knowing which is happening.
///
/// Tripping the handle only ever cancels; it is the caller's business to reset it before running
/// anything again.
pub fn install(cancel: CancelHandle) -> io::Result<()> {
    let executor = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    // A listener registers itself the first time it is polled, and until then the process still
    // dies on Ctrl-C. Polling it once here, on the calling thread, means the handler is standing
    // before this returns, rather than whenever the new thread gets to it.
    let mut interrupts = {
        let _context = executor.enter();
        let mut interrupts = Box::pin(tokio::signal::ctrl_c());
        let registered = interrupts
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()));
        if let Poll::Ready(Err(e)) = registered {
            return Err(e);
        }
        interrupts
    };

    std::thread::Builder::new()
        .name("avarice-interrupt".to_string())
        .spawn(move || {
            executor.block_on(async move {
                while interrupts.as_mut().await.is_ok() {
                    cancel.cancel();
                    interrupts.set(tokio::signal::ctrl_c());
                }
            })
        })?;
    Ok(())
}
