//! `/swarm`'s golden (TB-5, AGE-664/667): the pure layout in `ui::swarm`,
//! over the same three-level leader → reviewer → coder run TB-2 scripted
//! for `swarm_trace.rs`'s own invariants.
//!
//! `UPDATE_GOLDENS=1` rewrites `tui_swarm_golden.txt`.

use std::path::{Path, PathBuf};

use chatty_core::services::swarm_trace::SwarmTrace;

use super::swarm_trace::leader_reviewer_coder;
use crate::ui::swarm::render;

fn golden_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/goldens")
        .join(name)
}

fn check_golden(name: &str, actual: &str) {
    let path = golden_path(name);
    if std::env::var_os("UPDATE_GOLDENS").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "{} is missing ({e}); run with UPDATE_GOLDENS=1",
            path.display()
        )
    });
    assert_eq!(
        actual,
        expected,
        "{} changed; if that is intended, rerun with UPDATE_GOLDENS=1",
        path.display()
    );
}

/// `/swarm` on the scripted three-level run: root, its reviewer, the
/// reviewer's coder, each with its own model, status and spend.
#[tokio::test]
async fn tui_swarm_golden() {
    let run = leader_reviewer_coder().await;
    let trace = SwarmTrace::from_edges(&run.edges, &run.events);
    check_golden("tui_swarm_golden.txt", &render(&trace));
}
