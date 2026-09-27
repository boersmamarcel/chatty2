//! `cargo test` builds a crate's binary only when an integration test needs
//! it. The swarm kit's unit tests (`src/participant/swarm_kit.rs`, AGE-632)
//! spawn that binary as real workers, so this test is what makes
//! `cargo test -p chatty-tui` build it before they run.

#[test]
fn the_worker_binary_starts() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_chatty-tui"))
        .arg("--help")
        .output()
        .expect("chatty-tui runs");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
