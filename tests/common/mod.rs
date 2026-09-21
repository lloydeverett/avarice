//! Helpers shared between the integration tests.

// Each test file uses some of these and not others.
#![allow(dead_code)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use avarice_rt::{Runtime, StdModule};

/// A write sink a test can read back: hand a clone to the runtime, keep one to inspect.
#[derive(Clone, Default)]
pub struct Buffer(Arc<Mutex<Vec<u8>>>);

impl Buffer {
    pub fn new() -> Self {
        Buffer::default()
    }

    /// Everything written so far, as text.
    pub fn contents(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

impl Write for Buffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A scratch directory that deletes itself.
pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new() -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = format!(
            "avarice-rt-test-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        let path = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(&path).unwrap();
        TempDir(path)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    /// Writes a file, creating any directories above it, and hands back its full path.
    pub fn write(&self, relative: &str, contents: &str) -> PathBuf {
        let path = self.0.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, contents).unwrap();
        path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Whether `require(name)` succeeds in `rt`.
pub fn requirable(rt: &Runtime, name: &str) -> bool {
    rt.block_on(rt.eval::<bool>(format!("return (pcall(require, '{name}'))"), "=test"))
        .unwrap()
}

/// What `stdlib()` returns in `rt`.
pub fn listed(rt: &Runtime) -> Vec<String> {
    rt.block_on(rt.eval::<Vec<String>>("return stdlib()", "=test"))
        .unwrap()
}

/// The names of the stdlib modules this build has, which is what trusted mode registers (ADR
/// 0007).
pub fn compiled_in() -> Vec<&'static str> {
    StdModule::ALL
        .iter()
        .copied()
        .map(StdModule::name)
        .collect()
}

/// `text` without its colour: every `ESC [ … m` removed.
pub fn plain(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            assert_eq!(chars.next(), Some('['), "not a colour sequence in {text:?}");
            let last = chars.by_ref().find(|c| !(c.is_ascii_digit() || *c == ';'));
            assert_eq!(last, Some('m'), "not a colour sequence in {text:?}");
        } else {
            out.push(c);
        }
    }
    out
}
