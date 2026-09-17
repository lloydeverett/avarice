//! The two sets of defaults a runtime can be built from.

use mlua::StdLib;

/// The memory ceiling [`Profile::Sandbox`] applies unless told otherwise.
pub const DEFAULT_SANDBOX_MEMORY_LIMIT: usize = 128 * 1024 * 1024;

/// A named set of defaults for constructing a runtime.
///
/// A profile is a starting point, not a constraint: [`RuntimeBuilder`](crate::RuntimeBuilder)
/// can override anything a profile sets, including opening libraries the sandbox withholds.
/// Profiles are plain values, so reconfiguring one runtime never affects the profile another
/// runtime is built from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Profile {
    /// For Lua code the embedder does not vouch for.
    ///
    /// Withholds the libraries through which Lua can reach outside its own computation — `io`,
    /// `os`, `package`, `debug` — along with the filesystem functions that hide in the base
    /// library (`dofile`, `loadfile`). Caps memory at
    /// [`DEFAULT_SANDBOX_MEMORY_LIMIT`], and refuses binary chunks, `load`'s `"b"` mode
    /// included.
    ///
    /// No time limit is set by default: a sandbox that is merely slow is a judgement call only
    /// the embedder can make, so ask for one with
    /// [`RuntimeBuilder::time_limit`](crate::RuntimeBuilder::time_limit).
    Sandbox,

    /// For Lua code the embedder vouches for.
    ///
    /// Everything the sandbox has plus `io` and `os`, with no memory or time limit and binary
    /// chunks allowed. Note that `os.exit` will end the host process, and that `package` is
    /// still absent — Lua never loads its own modules here either.
    Trusted,
}

impl Profile {
    /// The standard libraries this profile opens.
    ///
    /// Never includes [`StdLib::PACKAGE`]: module loading is the host's business, so `require`
    /// is ours rather than Lua's. Never includes [`StdLib::DEBUG`] either — mlua refuses it on
    /// a safe state, because parts of it can violate the invariants mlua's own safety rests on.
    /// A `debug` table carrying just `traceback` is installed in its place.
    pub fn std_libs(self) -> StdLib {
        let common =
            StdLib::COROUTINE | StdLib::TABLE | StdLib::STRING | StdLib::UTF8 | StdLib::MATH;
        match self {
            Profile::Sandbox => common,
            Profile::Trusted => common | StdLib::IO | StdLib::OS,
        }
    }

    /// The memory ceiling this profile applies, if any.
    pub fn memory_limit(self) -> Option<usize> {
        match self {
            Profile::Sandbox => Some(DEFAULT_SANDBOX_MEMORY_LIMIT),
            Profile::Trusted => None,
        }
    }

    /// Whether this profile lets Lua load precompiled chunks.
    ///
    /// The sandbox does not: Lua's bytecode verifier is not a security boundary, and a
    /// handcrafted chunk can corrupt the VM.
    pub fn allows_binary_chunks(self) -> bool {
        match self {
            Profile::Sandbox => false,
            Profile::Trusted => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_profile_opens_package_or_debug() {
        for profile in [Profile::Sandbox, Profile::Trusted] {
            let libs = profile.std_libs();
            assert!(!libs.contains(StdLib::PACKAGE), "{profile:?} opens package");
            assert!(!libs.contains(StdLib::DEBUG), "{profile:?} opens debug");
        }
    }

    #[test]
    fn sandbox_withholds_io_and_os() {
        let libs = Profile::Sandbox.std_libs();
        assert!(!libs.contains(StdLib::IO));
        assert!(!libs.contains(StdLib::OS));
        assert!(libs.contains(StdLib::STRING));
    }
}
