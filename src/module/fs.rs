//! The filesystem [`ModuleStore`].

use std::fmt::Write as _;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use super::{ModuleName, ModuleSource, ModuleStore};
use crate::error::StoreError;

/// A [`ModuleStore`] that reads Lua source from directories on disk.
///
/// A name's segments become path components under each root in turn: `app.util` is looked for at
/// `<root>/app/util.lua` and then `<root>/app/util/init.lua`, with the roots tried in the order
/// they were added. Because [`ModuleName`] has already rejected `/`, `\` and `..`, no name can
/// address a file outside a root — though a symlink placed inside one still can, exactly as it
/// would for any other program reading those directories.
///
/// This lives in the library rather than in `avarice`: an embedder can ask for filesystem
/// resolution without going through the command-line program.
#[derive(Debug, Clone, Default)]
pub struct FsStore {
    roots: Vec<PathBuf>,
}

impl FsStore {
    /// A store with no roots, which finds nothing.
    pub fn empty() -> Self {
        FsStore::default()
    }

    /// A store rooted at one directory.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        FsStore {
            roots: vec![root.into()],
        }
    }

    /// A store that searches several directories, in order.
    pub fn with_roots<I, P>(roots: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        FsStore {
            roots: roots.into_iter().map(Into::into).collect(),
        }
    }

    /// Appends a directory to search after the ones already added.
    pub fn add_root(&mut self, root: impl Into<PathBuf>) -> &mut Self {
        self.roots.push(root.into());
        self
    }

    /// The directories this store searches, in order.
    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// The files that would be tried for `name`, in order.
    pub fn candidates(&self, name: &ModuleName) -> Vec<PathBuf> {
        let mut candidates = Vec::with_capacity(self.roots.len() * 2);
        for root in &self.roots {
            let mut base = root.clone();
            base.extend(name.segments());
            candidates.push(base.with_extension("lua"));
            candidates.push(base.join("init.lua"));
        }
        candidates
    }
}

impl ModuleStore for FsStore {
    fn fetch(&self, name: &ModuleName) -> Result<Option<ModuleSource>, StoreError> {
        for candidate in self.candidates(name) {
            match std::fs::read(&candidate) {
                Ok(source) => {
                    return Ok(Some(ModuleSource::new(source, display(&candidate))));
                }
                // A missing file, or a directory where we hoped for a file, just means "try the
                // next candidate". Anything else — a permissions problem, say — is worth
                // reporting rather than silently turning into "module not found".
                Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::IsADirectory) => {}
                Err(e) => return Err(StoreError::Io(e)),
            }
        }
        Ok(None)
    }

    fn describe(&self) -> String {
        if self.roots.is_empty() {
            return "filesystem store with no roots".to_string();
        }
        let mut out = String::from("filesystem store rooted at ");
        for (i, root) in self.roots.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            let _ = write!(out, "{}", display(root));
        }
        out
    }
}

fn display(path: &Path) -> String {
    path.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidates_follow_the_name_hierarchy() {
        let store = FsStore::new("/lua");
        let candidates = store.candidates(&ModuleName::new("app.util").unwrap());
        assert_eq!(
            candidates,
            vec![
                PathBuf::from("/lua/app/util.lua"),
                PathBuf::from("/lua/app/util/init.lua"),
            ]
        );
    }

    #[test]
    fn candidates_do_not_clobber_dots_in_the_last_segment() {
        // `with_extension` replaces an existing extension, so a segment must never look like
        // one. Name validation forbids dots inside a segment, which is what keeps this honest.
        let store = FsStore::new("/lua");
        let candidates = store.candidates(&ModuleName::new("a_b").unwrap());
        assert_eq!(candidates[0], PathBuf::from("/lua/a_b.lua"));
    }

    #[test]
    fn roots_are_searched_in_order() {
        let store = FsStore::with_roots(["/first", "/second"]);
        let candidates = store.candidates(&ModuleName::new("m").unwrap());
        assert_eq!(
            candidates,
            vec![
                PathBuf::from("/first/m.lua"),
                PathBuf::from("/first/m/init.lua"),
                PathBuf::from("/second/m.lua"),
                PathBuf::from("/second/m/init.lua"),
            ]
        );
    }

    #[test]
    fn empty_store_finds_nothing() {
        let store = FsStore::empty();
        let found = store.fetch(&ModuleName::new("m").unwrap()).unwrap();
        assert!(found.is_none());
    }
}
