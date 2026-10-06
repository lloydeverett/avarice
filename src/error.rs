//! Error types for runtime setup and for limits tripping.

/// An error from avarice itself, as opposed to from Lua.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// An error raised by Lua, or by mlua on Lua's behalf.
    #[error(transparent)]
    Lua(#[from] mlua::Error),

    /// A module name was not well formed.
    #[error(transparent)]
    ModuleName(#[from] InvalidModuleName),

    /// A [`ModuleStore`](crate::ModuleStore) failed to answer.
    #[error("module store failed: {0}")]
    Store(#[from] StoreError),

    /// The runtime could not be built as configured.
    #[error("{0}")]
    Config(String),
}

/// Shorthand for results carrying an avarice [`Error`].
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Why a string was rejected as a module name.
///
/// Module names are validated before they reach a [`ModuleStore`](crate::ModuleStore), so a
/// store never has to defend itself against `../` or an absolute path.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum InvalidModuleName {
    #[error("module name is empty")]
    Empty,
    #[error("module name is longer than {max} bytes")]
    TooLong { max: usize },
    #[error("module name has more than {max} dot-separated segments")]
    TooDeep { max: usize },
    #[error("module name has an empty segment")]
    EmptySegment,
    #[error("module name segment starts with a digit")]
    LeadingDigit,
    #[error("module name contains {0:?}, which is not a letter, digit or underscore")]
    BadCharacter(char),
}

/// An error from a [`ModuleStore`](crate::ModuleStore).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StoreError {
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// Anything else a store wants to report.
    #[error(transparent)]
    Other(Box<dyn std::error::Error + Send + Sync>),
}

impl StoreError {
    /// Wraps an arbitrary error as a store failure.
    pub fn other(err: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> Self {
        StoreError::Other(err.into())
    }
}

/// The error raised inside Lua when a [`CancelHandle`](crate::CancelHandle) is tripped.
///
/// Reaches Rust as [`mlua::Error::CallbackError`] wrapping an
/// [`mlua::Error::ExternalError`]; use [`was_cancelled`](crate::was_cancelled) to recognise it
/// rather than matching on the message.
#[derive(Debug, Clone, Copy, thiserror::Error)]
#[error("execution cancelled")]
pub struct Cancelled;

/// The error raised inside Lua when a time limit is exceeded.
#[derive(Debug, Clone, Copy, thiserror::Error)]
#[error("execution time limit exceeded")]
pub struct TimedOut;

/// Whether this error is the result of cancelling execution.
pub fn was_cancelled(err: &mlua::Error) -> bool {
    has_external::<Cancelled>(err)
}

/// Whether this error is the result of a time limit being exceeded.
pub fn was_timed_out(err: &mlua::Error) -> bool {
    has_external::<TimedOut>(err)
}

/// Whether this error is the result of the memory limit being exceeded.
pub fn was_out_of_memory(err: &mlua::Error) -> bool {
    let mut err = err;
    loop {
        match err {
            mlua::Error::MemoryError(_) => return true,
            mlua::Error::CallbackError { cause, .. } => err = cause,
            mlua::Error::WithContext { cause, .. } => err = cause,
            _ => return false,
        }
    }
}

fn has_external<T: std::error::Error + 'static>(err: &mlua::Error) -> bool {
    let mut err = err;
    loop {
        match err {
            mlua::Error::ExternalError(e) => return e.downcast_ref::<T>().is_some(),
            mlua::Error::CallbackError { cause, .. } => err = cause,
            mlua::Error::WithContext { cause, .. } => err = cause,
            _ => return false,
        }
    }
}
