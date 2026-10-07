//! Runs the example, and checks it did what its comments say.

use std::process::Command;

#[test]
fn the_example_runs_and_prints_what_it_says() {
    let output = Command::new(env!("CARGO_BIN_EXE_example-app"))
        .output()
        .expect("the example could not be started");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "stdout:\n{stdout}\nstderr:\n{stderr}"
    );

    // From the contributed module, its Rust half and its Lua half.
    assert!(stdout.contains("Hello, world!"), "{stdout}");
    assert!(stdout.contains("Hello, Ada!\nHello, Grace!"), "{stdout}");
    // From the module store, using a stdlib module.
    assert!(stdout.contains("HELLO, WORLD!"), "{stdout}");
    // A value handed back to Rust: the SHA-256 of "avarice".
    assert!(stdout.contains("digest: a1488806"), "{stdout}");
    // The sandbox's memory cap, caught from Rust.
    assert!(stdout.contains("refused: out of memory"), "{stdout}");
}
