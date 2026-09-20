//! Module names, and the store `require` fetches source from.

mod fs;

pub use fs::FsStore;

use std::fmt;
use std::str::FromStr;

use crate::error::{InvalidModuleName, StoreError};

/// The longest module name accepted, in bytes.
pub const MAX_NAME_LEN: usize = 255;

/// The deepest module name accepted, in dot-separated segments.
pub const MAX_NAME_SEGMENTS: usize = 16;

/// A validated module name, such as `json` or `app.util.strings`.
///
/// The hierarchy is a naming convention, not a path: a store is free to map `app.util` onto a
/// directory, a database row, or anything else. Validation happens here, once, so that no store
/// has to think about `..`, `/`, or a leading `~`.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ModuleName(String);

impl ModuleName {
    /// Validates `name` as a module name.
    ///
    /// A name is one or more dot-separated segments. Each segment starts with an ASCII letter or
    /// an underscore and continues with ASCII letters, digits or underscores.
    pub fn new(name: impl Into<String>) -> Result<Self, InvalidModuleName> {
        let name = name.into();
        if name.is_empty() {
            return Err(InvalidModuleName::Empty);
        }
        if name.len() > MAX_NAME_LEN {
            return Err(InvalidModuleName::TooLong { max: MAX_NAME_LEN });
        }
        let mut segments = 0;
        for segment in name.split('.') {
            segments += 1;
            if segments > MAX_NAME_SEGMENTS {
                return Err(InvalidModuleName::TooDeep {
                    max: MAX_NAME_SEGMENTS,
                });
            }
            let mut chars = segment.chars();
            match chars.next() {
                None => return Err(InvalidModuleName::EmptySegment),
                Some(c) if c.is_ascii_digit() => return Err(InvalidModuleName::LeadingDigit),
                Some(c) if !is_name_char(c) => return Err(InvalidModuleName::BadCharacter(c)),
                Some(_) => {}
            }
            if let Some(c) = chars.find(|&c| !is_name_char(c)) {
                return Err(InvalidModuleName::BadCharacter(c));
            }
        }
        Ok(ModuleName(name))
    }

    /// The name as written, dots included.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The name's segments, outermost first.
    ///
    /// Always yields at least one segment, and never an empty one.
    pub fn segments(&self) -> impl Iterator<Item = &str> {
        self.0.split('.')
    }
}

fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

impl fmt::Display for ModuleName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for ModuleName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, f)
    }
}

impl AsRef<str> for ModuleName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl FromStr for ModuleName {
    type Err = InvalidModuleName;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        ModuleName::new(s)
    }
}

impl From<ModuleName> for String {
    fn from(name: ModuleName) -> String {
        name.0
    }
}

/// Lua source for one module, along with where it came from.
#[derive(Clone)]
pub struct ModuleSource {
    source: Vec<u8>,
    origin: String,
}

impl ModuleSource {
    /// Builds a source.
    ///
    /// `origin` names where the source came from, and appears in error messages and tracebacks —
    /// a file path for [`FsStore`], but a store is free to use a URL, a row id, or anything else
    /// that helps a reader find the code again.
    pub fn new(source: impl Into<Vec<u8>>, origin: impl Into<String>) -> Self {
        ModuleSource {
            source: source.into(),
            origin: origin.into(),
        }
    }

    /// The Lua source text.
    pub fn source(&self) -> &[u8] {
        &self.source
    }

    /// Where the source came from.
    pub fn origin(&self) -> &str {
        &self.origin
    }
}

impl fmt::Debug for ModuleSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ModuleSource")
            .field("origin", &self.origin)
            .field("source", &format_args!("<{} bytes>", self.source.len()))
            .finish()
    }
}

/// Where a program's Lua source lives.
///
/// Answers one question: given a module name, produce source or nothing. The filesystem is one
/// store ([`FsStore`]) and currently the only one shipped, but nothing above this trait assumes
/// a filesystem — a store backed by a database table or an archive is equally valid.
///
/// Stores are `Send + Sync` so that one store can back several runtimes, and because a runtime
/// holds its store where Lua's own `require` reaches it. A store that cannot meet that bound can
/// be wrapped in a mutex.
pub trait ModuleStore: Send + Sync + 'static {
    /// Fetches source for `name`, or `Ok(None)` if this store does not have it.
    ///
    /// `Ok(None)` and `Err` mean different things: the first lets `require` report a plain
    /// "module not found", the second surfaces as an error from `require` itself.
    fn fetch(&self, name: &ModuleName) -> Result<Option<ModuleSource>, StoreError>;

    /// A short phrase naming this store, used in "module not found" messages.
    fn describe(&self) -> String {
        "module store".to_string()
    }
}

impl<T: ModuleStore + ?Sized> ModuleStore for std::sync::Arc<T> {
    fn fetch(&self, name: &ModuleName) -> Result<Option<ModuleSource>, StoreError> {
        (**self).fetch(name)
    }

    fn describe(&self) -> String {
        (**self).describe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_plain_and_dotted_names() {
        assert_eq!(ModuleName::new("json").unwrap().as_str(), "json");
        let name = ModuleName::new("app.util.strings").unwrap();
        assert_eq!(
            name.segments().collect::<Vec<_>>(),
            ["app", "util", "strings"]
        );
        assert!(ModuleName::new("_private").is_ok());
        assert!(ModuleName::new("utf8").is_ok());
    }

    #[test]
    fn rejects_path_traversal_and_separators() {
        for bad in [
            "..",
            "../etc/passwd",
            "/etc/passwd",
            "a/b",
            "a\\b",
            "a b",
            "a-b",
            "a\0b",
            "café",
        ] {
            assert!(
                matches!(
                    ModuleName::new(bad),
                    Err(InvalidModuleName::BadCharacter(_) | InvalidModuleName::EmptySegment)
                ),
                "{bad:?} should have been rejected"
            );
        }
    }

    #[test]
    fn rejects_empty_and_malformed_segments() {
        assert_eq!(ModuleName::new(""), Err(InvalidModuleName::Empty));
        assert_eq!(ModuleName::new("."), Err(InvalidModuleName::EmptySegment));
        assert_eq!(ModuleName::new("a."), Err(InvalidModuleName::EmptySegment));
        assert_eq!(ModuleName::new(".a"), Err(InvalidModuleName::EmptySegment));
        assert_eq!(
            ModuleName::new("a..b"),
            Err(InvalidModuleName::EmptySegment)
        );
        assert_eq!(ModuleName::new("1a"), Err(InvalidModuleName::LeadingDigit));
        assert_eq!(
            ModuleName::new("a.2b"),
            Err(InvalidModuleName::LeadingDigit)
        );
    }

    #[test]
    fn rejects_oversized_names() {
        let long = "a".repeat(MAX_NAME_LEN + 1);
        assert_eq!(
            ModuleName::new(long),
            Err(InvalidModuleName::TooLong { max: MAX_NAME_LEN })
        );
        let deep = vec!["a"; MAX_NAME_SEGMENTS + 1].join(".");
        assert_eq!(
            ModuleName::new(deep),
            Err(InvalidModuleName::TooDeep {
                max: MAX_NAME_SEGMENTS
            })
        );
        let at_limit = vec!["a"; MAX_NAME_SEGMENTS].join(".");
        assert!(ModuleName::new(at_limit).is_ok());
    }
}
