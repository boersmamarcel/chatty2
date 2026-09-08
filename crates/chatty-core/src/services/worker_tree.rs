//! ADR-0012's worker isolation on the desktop: a `git worktree` per worker.
//!
//! The broker's local runner spawns each worker behind an A2A endpoint
//! (AGE-301) and gives it a tree of its own, so two workers on one
//! conversation cannot overwrite each other's edits (AGE-314).
//!
//! # What a worker gets
//!
//! Its own checkout at `<workspace>/.chatty/worktrees/<name>` on its own
//! `sub-agent/<name>` branch. Two workers editing one file then produce a git
//! conflict the leader can see, rather than a last-writer-wins result neither
//! of them reported.
//!
//! # What it does not get
//!
//! Automatic cleanup. The worktree is left on disk with the worker's output
//! committed to its branch; removing it is a separate step after a merge.
//! That is deliberate — `git worktree remove` refusing a dirty tree is the
//! only guard against destroying work no commit has captured.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Result;
use tracing::{info, warn};

use crate::services::git_service::GitService;

/// A worker's isolated checkout.
#[derive(Clone, Debug)]
pub struct WorkerTree {
    /// Worktree directory name under `WORKTREE_DIR`.
    pub name: String,
    /// The branch created for it, where its output is committed.
    pub branch: String,
    /// Absolute path — the worker's `cwd` and the root its tools are
    /// confined to.
    pub path: PathBuf,
}

/// Monotonic suffix so two workers started in the same millisecond cannot
/// collide on a directory name.
static WORKER_SEQ: AtomicU64 = AtomicU64::new(0);

/// A directory name no other worker will take.
pub fn next_worker_name(prefix: &str) -> String {
    let seq = WORKER_SEQ.fetch_add(1, Ordering::Relaxed);
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("{prefix}{stamp}-{seq}")
}

/// Create `name`'s worktree under `workspace_root`.
///
/// `Ok(None)` means no isolation was available and the caller should fall
/// back to the shared tree: without a git repository there is no branch to
/// hand back and no conflict to surface, so parallel workers collide exactly
/// as they did before. AGE-314 carries the open question of whether a
/// non-git workspace should instead refuse to fan out.
pub async fn create(workspace_root: &str, name: &str) -> Result<Option<WorkerTree>> {
    let git = match GitService::new(workspace_root).await {
        Ok(git) => git,
        Err(e) => {
            warn!(
                workspace = %workspace_root,
                error = %e,
                "No worktree isolation: the workspace is not a git repository, \
                 so parallel workers share one tree"
            );
            return Ok(None);
        }
    };

    let branch = format!("sub-agent/{name}");
    match git.worktree_add(name, &branch).await {
        Ok(path) => {
            info!(worktree = %path.display(), branch = %branch, "Worker isolated in a worktree");
            Ok(Some(WorkerTree {
                name: name.to_string(),
                branch,
                path,
            }))
        }
        Err(e) => {
            warn!(error = %e, "No worktree isolation: falling back to the shared tree");
            Ok(None)
        }
    }
}

/// Commit whatever the worker left behind onto its branch.
///
/// The worker's turn is not over until its output is durable. This runs
/// whether the worker succeeded or failed: a failed worker's partial edits
/// are still the only copy that exists. Failures are logged, never fatal —
/// the worktree stays on disk either way, so nothing is destroyed by one.
pub async fn commit(tree: &WorkerTree, task: &str) {
    let git = match GitService::new(&tree.path.to_string_lossy()).await {
        Ok(git) => git,
        Err(e) => {
            warn!(worktree = %tree.name, error = %e, "Cannot open the worker's worktree to commit it");
            return;
        }
    };

    let summary: String = task.chars().take(72).collect();
    let message = format!("sub-agent {}: {}", tree.name, summary);

    match git.commit_all(&message).await {
        Ok(Some(commit)) => {
            info!(branch = %tree.branch, hash = %commit.hash, "Worker output committed")
        }
        Ok(None) => info!(branch = %tree.branch, "Worker made no changes"),
        Err(e) => warn!(branch = %tree.branch, error = %e, "Failed to commit worker output"),
    }
}

/// The sentence a leader is told so it knows the output is on a branch
/// rather than in its own tree. Nothing is merged automatically.
pub fn merge_hint(tree: &WorkerTree) -> String {
    format!(
        "\n\n[Worker output is on branch '{}'. Merge it to take the changes; \
         its worktree is left in place until then.]",
        tree.branch
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_names_are_unique_within_a_process() {
        let a = next_worker_name("w");
        let b = next_worker_name("w");
        assert_ne!(a, b, "two workers must not share a directory");
        assert!(a.starts_with('w') && b.starts_with('w'));
    }

    #[tokio::test]
    async fn a_non_git_workspace_falls_back_rather_than_failing() {
        let dir = tempfile::tempdir().unwrap();
        let result = create(&dir.path().to_string_lossy(), "w1").await;
        assert!(
            matches!(result, Ok(None)),
            "no repository means no isolation, not a failed delegation"
        );
    }

    #[test]
    fn the_merge_hint_names_the_branch_the_leader_has_to_take() {
        let tree = WorkerTree {
            name: "w1".into(),
            branch: "sub-agent/w1".into(),
            path: PathBuf::from("/tmp/w1"),
        };
        assert!(merge_hint(&tree).contains("sub-agent/w1"));
    }
}
