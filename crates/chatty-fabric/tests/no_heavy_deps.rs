//! `chatty-fabric` is the pure core both the gateway and chatty-core depend
//! on. If it ever pulls in a server, a WASM engine, the hive client, the UI
//! toolkit or an HTTP client, that stops being true without anyone deciding
//! it. This reads the resolved dependency graph and fails if it does.

use std::collections::{HashMap, HashSet, VecDeque};
use std::process::Command;

use serde_json::Value;

const FORBIDDEN: [&str; 5] = ["axum", "wasmtime", "hive-client", "gpui", "reqwest"];

fn is_forbidden(name: &str) -> bool {
    FORBIDDEN.iter().any(|f| {
        name == *f
            || name
                .strip_prefix(f)
                .is_some_and(|rest| rest.starts_with('-') || rest.starts_with('_'))
    })
}

#[test]
fn fabric_has_no_heavy_deps() {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let out = Command::new(cargo)
        .args([
            "metadata",
            "--format-version",
            "1",
            "--offline",
            "--manifest-path",
        ])
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
        .output()
        .expect("run cargo metadata");
    assert!(
        out.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let meta: Value = serde_json::from_slice(&out.stdout).expect("cargo metadata is JSON");

    let names: HashMap<&str, &str> = meta["packages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| (p["id"].as_str().unwrap(), p["name"].as_str().unwrap()))
        .collect();
    let nodes: HashMap<&str, &Value> = meta["resolve"]["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| (n["id"].as_str().unwrap(), n))
        .collect();
    let root = names
        .iter()
        .find(|(_, name)| **name == "chatty-fabric")
        .map(|(id, _)| *id)
        .expect("chatty-fabric is in the workspace");

    // Walk normal (non-dev, non-build) edges only: what ships.
    let mut seen = HashSet::from([root]);
    let mut queue = VecDeque::from([(root, vec!["chatty-fabric"])]);
    let mut offenders = Vec::new();
    while let Some((id, path)) = queue.pop_front() {
        for dep in nodes[id]["deps"].as_array().unwrap() {
            let normal = dep["dep_kinds"]
                .as_array()
                .unwrap()
                .iter()
                .any(|k| k["kind"].is_null());
            let dep_id = dep["pkg"].as_str().unwrap();
            if !normal || !seen.insert(dep_id) {
                continue;
            }
            let name = names[dep_id];
            let mut path = path.clone();
            path.push(name);
            if is_forbidden(name) {
                offenders.push(path.join(" -> "));
            }
            queue.push_back((dep_id, path));
        }
    }
    assert!(seen.len() > 1, "the walk found no dependencies at all");
    assert!(
        offenders.is_empty(),
        "chatty-fabric must stay pure, but depends on:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn the_forbidden_match_covers_sub_crates_only() {
    assert!(is_forbidden("axum"));
    assert!(is_forbidden("axum-core"));
    assert!(is_forbidden("wasmtime-environ"));
    assert!(is_forbidden("gpui_macros"));
    assert!(!is_forbidden("reqwestish"));
    assert!(!is_forbidden("serde"));
}
