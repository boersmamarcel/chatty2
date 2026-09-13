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
//! The participant name is unique per broker socket, but branches and
//! worktree paths live in one repository shared by every broker on it: a
//! sub-leader started with `--broker` names its workers from a counter of
//! its own, and a leftover tree from an earlier run keeps its branch
//! (AGE-402). So the name is taken as a wish: when `sub-agent/<name>` or the
//! directory already exists, the tree is `<name>-2`, `<name>-3`, … instead.
//! The merge hint the leader receives names the branch actually created.
//!
//! # What it does not get
//!
//! Automatic cleanup. The worktree is left on disk with the worker's output
//! committed to its branch; removing it is a separate step after a merge.
//! That is deliberate — `git worktree remove` refusing a dirty tree is the
//! only guard against destroying work no commit has captured.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, bail};
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

/// How many `<name>-N` suffixes are tried before giving up. Far more than
/// any repository will accumulate; the bound only keeps a broken repository
/// from looping.
const MAX_NAME_ATTEMPTS: u32 = 1000;

/// Create a worktree for the worker called `name` under `workspace_root`.
///
/// `Ok(None)` means no isolation was available and the caller should fall
/// back to the shared tree: without a git repository there is no branch to
/// hand back and no conflict to surface, so parallel workers collide exactly
/// as they did before. AGE-314 carries the open question of whether a
/// non-git workspace should instead refuse to fan out.
///
/// In a git repository the tree is either created or the delegation fails:
/// a worker that silently ran in the shared tree would report a branch that
/// does not exist (AGE-402). The tree's name is `name` unless the repository
/// already has that branch or directory — another broker's worker, or a
/// tree left from an earlier run — in which case it is the first free
/// `name-N`.
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

    let tree = add_worktree(&git, name)
        .await
        .with_context(|| format!("cannot isolate worker '{name}' in a worktree"))?;
    info!(worktree = %tree.path.display(), branch = %tree.branch, "Worker isolated in a worktree");
    Ok(Some(tree))
}

async fn add_worktree(git: &GitService, name: &str) -> Result<WorkerTree> {
    let tree_name = free_worktree_name(git, name).await?;
    let branch = format!("sub-agent/{tree_name}");
    let path = git.worktree_add(&tree_name, &branch).await?;
    Ok(WorkerTree {
        name: tree_name,
        branch,
        path,
    })
}

/// `name`, or the first `name-N` (from 2) whose branch and directory are
/// both free in this repository.
async fn free_worktree_name(git: &GitService, name: &str) -> Result<String> {
    for attempt in 1..=MAX_NAME_ATTEMPTS {
        let candidate = if attempt == 1 {
            name.to_string()
        } else {
            format!("{name}-{attempt}")
        };
        let branch = format!("sub-agent/{candidate}");
        if git.worktree_path(&candidate).exists() {
            continue;
        }
        if git.branch_exists(&branch).await? {
            continue;
        }
        if attempt > 1 {
            info!(
                worker = %name,
                worktree = %candidate,
                "Worker name is taken in this repository; using a free one"
            );
        }
        return Ok(candidate);
    }
    bail!("no free worktree name for worker '{name}' after {MAX_NAME_ATTEMPTS} attempts")
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

/// What runs when the worker's process exits, given whether its task
/// succeeded.
///
/// Mirrors the shape `chatty-protocol-gateway`'s `WorkerWorkspace::on_exit`
/// needs. This crate does not depend on that one — its optional `worker`
/// feature depends back on `chatty-core`, so the edge stays acyclic — which
/// is why [`create_with_commit_hook`] returns this instead of a
/// `WorkerWorkspace` directly; the caller (chatty-gpui's `broker_runner`,
/// chatty-tui's `--broker` wiring) wraps it in a couple of lines.
pub type ExitHook = Box<dyn FnOnce(bool) + Send>;

/// [`create`] plus the commit-on-exit hook every caller wants: the whole
/// body of a broker's git-worktree `WorkspaceFactory`, minus the wrapping
/// into that type (AGE-376). Also returns [`merge_hint`], since a caller
/// with a `WorkerTree` has everything needed to hand the leader the note
/// naming the branch its output landed on (AGE-399).
pub async fn create_with_commit_hook(
    workspace_root: &str,
    worker: &str,
) -> Result<Option<(PathBuf, String, ExitHook)>> {
    let Some(tree) = create(workspace_root, worker).await? else {
        return Ok(None);
    };
    let cwd = tree.path.clone();
    let hint = merge_hint(&tree);
    let on_exit: ExitHook = Box::new(move |_succeeded| {
        // `on_exit` runs from the runner's `Drop`, which cannot await;
        // committing is a `git` subprocess, so it is spawned. The tree is
        // the worker's alone, so nothing races this.
        tokio::spawn(async move {
            // Committed whether the worker succeeded or not: a failed
            // worker's partial edits are still the only copy that exists.
            commit(&tree, "delegated task").await;
        });
    });
    Ok(Some((cwd, hint, on_exit)))
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

    async fn git(args: &[&str], dir: &std::path::Path) {
        let out = tokio::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .await
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// A repository with one commit, so worktrees can be added.
    async fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        git(&["init", "-q"], dir.path()).await;
        git(&["config", "user.email", "t@example.com"], dir.path()).await;
        git(&["config", "user.name", "T"], dir.path()).await;
        std::fs::write(dir.path().join("README"), "hi").unwrap();
        git(&["add", "."], dir.path()).await;
        git(&["commit", "-q", "-m", "init"], dir.path()).await;
        dir
    }

    /// AGE-402: two brokers on one repository both name their first worker
    /// `local-coder-0`. Both get a tree and a branch of their own.
    #[tokio::test]
    async fn the_same_worker_name_twice_gets_two_branches() {
        let dir = repo().await;
        let root = dir.path().to_string_lossy().to_string();

        let first = create(&root, "local-coder-0").await.unwrap().unwrap();
        let second = create(&root, "local-coder-0").await.unwrap().unwrap();

        assert_eq!(first.branch, "sub-agent/local-coder-0");
        assert_eq!(second.branch, "sub-agent/local-coder-0-2");
        assert_ne!(first.path, second.path);
        assert!(first.path.is_dir() && second.path.is_dir());
        assert!(
            merge_hint(&second).contains("sub-agent/local-coder-0-2"),
            "the leader is told the branch that exists, not the one it asked for"
        );
    }

    /// The nested shape from the issue: a sub-leader's broker runs inside
    /// its own worktree and names its worker the same as the top leader
    /// does. Branches are repository-wide, so the nested one must yield.
    #[tokio::test]
    async fn a_nested_broker_does_not_take_the_top_level_branch() {
        let dir = repo().await;
        let root = dir.path().to_string_lossy().to_string();

        let lead = create(&root, "local-lead-0").await.unwrap().unwrap();
        let nested_root = lead.path.to_string_lossy().to_string();
        let nested = create(&nested_root, "local-coder-0")
            .await
            .unwrap()
            .unwrap();
        let top = create(&root, "local-coder-0").await.unwrap().unwrap();

        assert_eq!(nested.branch, "sub-agent/local-coder-0");
        assert!(nested.path.starts_with(&lead.path));
        assert_eq!(top.branch, "sub-agent/local-coder-0-2");
        assert!(top.path.starts_with(dir.path()) && !top.path.starts_with(&lead.path));
    }

    /// A tree left behind by an earlier run — same name, its branch still
    /// there — no longer sends the new worker into the shared tree.
    #[tokio::test]
    async fn a_leftover_tree_from_an_earlier_run_is_skipped() {
        let dir = repo().await;
        let root = dir.path().to_string_lossy().to_string();

        let earlier = create(&root, "local-coder-0").await.unwrap().unwrap();
        // The directory is gone but the branch remains, as after a manual
        // `rm -rf` of `.chatty/worktrees`.
        git(
            &[
                "worktree",
                "remove",
                "--force",
                &earlier.path.to_string_lossy(),
            ],
            dir.path(),
        )
        .await;
        let again = create(&root, "local-coder-0").await.unwrap().unwrap();
        assert_eq!(again.branch, "sub-agent/local-coder-0-2");
    }

    /// In a repository, a worktree that cannot be made fails the delegation
    /// rather than quietly running the worker in the shared tree.
    #[tokio::test]
    async fn a_failed_worktree_is_an_error_not_a_shared_tree() {
        let dir = repo().await;
        let root = dir.path().to_string_lossy().to_string();

        let err = create(&root, "../escape").await.unwrap_err();
        assert!(
            format!("{err:#}").contains("cannot isolate worker '../escape'"),
            "{err:#}"
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
