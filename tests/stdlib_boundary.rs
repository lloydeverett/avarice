//! The stdlib is a directory of the crate and not a crate of its own (ADR 0012), so nothing but a
//! test stops it reaching into the rest of `avarice-rt`. This is that test: no source under
//! `src/stdlib` names another module of the crate.
//!
//! The one thing it may name is `crate::components`, which Astra's `http` files spell that way and
//! are kept exactly as Astra wrote them (ADR 0006); `src/lib.rs` makes the name resolve.

use std::fs;
use std::path::{Path, PathBuf};

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("src/stdlib is readable") {
        let path = entry.expect("directory entry").path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn the_stdlib_names_nothing_of_the_crate_outside_itself() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/stdlib");
    let mut files = Vec::new();
    rust_files(&root, &mut files);
    assert!(
        !files.is_empty(),
        "found no sources under {}",
        root.display()
    );

    let mut offenders = Vec::new();
    for file in files {
        let source = fs::read_to_string(&file).expect("source is UTF-8");
        for (number, line) in source.lines().enumerate() {
            // Comments and doc links may mention anything; only code has to stay inside.
            if line.trim_start().starts_with("//") {
                continue;
            }
            for (at, _) in line.match_indices("crate::") {
                let path = &line[at + "crate::".len()..];
                if !(path.starts_with("stdlib::") || path.starts_with("components")) {
                    offenders.push(format!(
                        "{}:{}: {}",
                        file.display(),
                        number + 1,
                        line.trim()
                    ));
                }
            }
            if line.contains("super::super::super") || line.contains("avarice_rt::") {
                offenders.push(format!(
                    "{}:{}: {}",
                    file.display(),
                    number + 1,
                    line.trim()
                ));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "the stdlib reaches outside itself:\n{}",
        offenders.join("\n")
    );
}
