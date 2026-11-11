//! `scripts/fabric/relay-rate.sh` over a fixture edge log (TM-3). Needs
//! `bash` and `jq`, as the script does.

#![cfg(unix)]

use std::path::Path;
use std::process::Command;

/// Whether `tool` runs; a machine without it skips the test.
fn has(tool: &str) -> bool {
    Command::new(tool)
        .arg("--version")
        .output()
        .is_ok_and(|out| out.status.success())
}

fn script_output(files: &[&str]) -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let out = Command::new("bash")
        .arg(root.join("scripts/fabric/relay-rate.sh"))
        .args(
            files
                .iter()
                .map(|f| root.join("scripts/fabric/fixtures").join(f)),
        )
        .output()
        .expect("bash runs");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// Five runs over two files: t-1 and t-5 hold a relay request, t-2 and t-4
/// are multi-worker without one, t-3 has one worker (its message names no
/// sibling, and a lone worker has none). Two relay messages in t-5 count the
/// run once; a repeat task row for x-1 is not a third worker.
#[test]
fn relay_rate_script_on_fixture() {
    for tool in ["bash", "jq"] {
        if !has(tool) {
            eprintln!("skipping: {tool} not found");
            return;
        }
    }
    assert_eq!(
        script_output(&["edges-relay-1.jsonl", "edges-relay-2.jsonl"]),
        "runs=5 multi_worker_runs=4 runs_with_relay=2 rate=0.500\n"
    );
    // One file alone: t-1 relays, t-2 does not, t-3 is single-worker.
    assert_eq!(
        script_output(&["edges-relay-1.jsonl"]),
        "runs=3 multi_worker_runs=2 runs_with_relay=1 rate=0.500\n"
    );
}
