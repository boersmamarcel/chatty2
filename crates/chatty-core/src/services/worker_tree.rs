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
//! The evidence envelope the leader receives names the branch actually
//! created.
//!
//! # What the leader is told
//!
//! Not the worker's word for it. When the task ends the runner commits the
//! tree and reads back the branch, the commit count, `git diff --stat`
//! against the default branch and — when the team declares one — the exit
//! code and last lines of a verification command run in the tree. That is
//! an [`Evidence`], rendered as a fenced `evidence` block and carried as a
//! structured field beside it (ADR-0011 C12, AGE-406). A branch with no
//! commits produces none at all: a read-only reviewer must not be handed
//! something to merge.
//!
//! # What it does not get
//!
//! Automatic cleanup. The worktree is left on disk with the worker's output
//! committed to its branch; removing it is a separate step after a merge.
//! That is deliberate — `git worktree remove` refusing a dirty tree is the
//! only guard against destroying work no commit has captured.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
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

/// Commits the worker's output and reports what it left behind, once.
///
/// Awaited by the runner at the task's terminal status, which is the only
/// moment where the tree is final *and* something is still listening: the
/// commit has to have happened or the count and the diff stat are stale,
/// and the answer has to be collected before the terminal event or the
/// caller has already stopped reading.
pub type EvidenceHook =
    Box<dyn FnOnce() -> Pin<Box<dyn Future<Output = Option<Evidence>> + Send>> + Send>;

/// How long the team's verification command gets before its process group
/// is killed.
///
/// Five minutes: long enough for a real unit-test suite, short enough that a
/// hung command does not hold the delegation open until the caller's own
/// HTTP timeout. The wait happens between the worker's last update and the
/// terminal event, so the caller sees nothing but the broker's 15 s SSE
/// keep-alives while it runs; the assertion below keeps it inside
/// [`DELEGATION_READ_TIMEOUT`](super::a2a_client::DELEGATION_READ_TIMEOUT)
/// anyway, so that a slow suite can never be the thing that looks like a
/// dead socket.
pub const VERIFICATION_TIMEOUT: Duration = Duration::from_secs(300);

const _: () = assert!(
    VERIFICATION_TIMEOUT.as_secs() < super::a2a_client::DELEGATION_READ_TIMEOUT.as_secs(),
    "a verification command must finish inside the delegation's silence \
     budget, or a slow suite fails the delegation as a dead socket"
);

/// How many lines of the verification command's output the envelope keeps.
const VERIFICATION_TAIL_LINES: usize = 20;

/// What the runner — not the worker — can say about a finished delegation
/// (ADR-0011 C12, AGE-406).
///
/// Leaders and reviewers trusted narrative over state: a report saying "all
/// 4 tests passing" over a branch with one inserted line, an APPROVE over a
/// test run with two failures. These are the facts that were in the tree
/// when the worker exited, collected by the process that owns the tree.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Evidence {
    /// The branch the worker's output was committed to.
    pub branch: String,
    /// What it is measured against: `main` or `master`.
    pub base: String,
    /// How many commits the branch carries that the base does not. Never
    /// zero — an envelope with no commits is not produced at all.
    pub commits: usize,
    /// `git diff --stat <base>..<branch>`, verbatim.
    pub diff_stat: String,
    /// The team's verification command, when one is declared and the
    /// worker's profile has a shell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification: Option<Verification>,
}

/// The team's verification command as the runner ran it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Verification {
    /// The command, as declared in `module_settings.team.verification`.
    pub command: String,
    /// Its exit code; `None` when it was killed by a signal or by
    /// [`VERIFICATION_TIMEOUT`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// Whether it timed out rather than finishing.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub timed_out: bool,
    /// The last [`VERIFICATION_TAIL_LINES`] lines of its combined output.
    pub output: String,
}

impl Evidence {
    /// The fenced block appended to the worker's answer.
    ///
    /// Fenced and titled `evidence` so a leader can tell it from the
    /// worker's own report at a glance: the block is the runner's, the
    /// prose above it is the worker's.
    pub fn block(&self) -> String {
        let mut text = format!(
            "\n\n```evidence\nbranch: {}\nbase: {}\ncommits: {}\ndiff --stat {}..{}:\n{}\n",
            self.branch, self.base, self.commits, self.base, self.branch, self.diff_stat
        );
        if let Some(check) = &self.verification {
            let outcome = if check.timed_out {
                "timed out".to_string()
            } else {
                match check.exit_code {
                    Some(code) => format!("exit code {code}"),
                    None => "killed by a signal".to_string(),
                }
            };
            text.push_str(&format!(
                "verification: {} — {outcome}\n{}\n",
                check.command, check.output
            ));
        }
        text.push_str("```");
        text
    }

    /// The same facts as JSON, for the A2A task result's structured field —
    /// so a trace, or Harbor's ATIF, reads them without parsing prose.
    pub fn json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }
}

/// [`create`] plus the two hooks a broker's git-worktree `WorkspaceFactory`
/// needs: the evidence envelope the runner awaits when the task ends
/// (AGE-406), and the commit-on-exit fallback for a task that never got
/// there (AGE-376).
///
/// `verification` is the team's command, already filtered by the caller for
/// a worker whose profile has no shell.
///
/// Both hooks commit, and only the first one to run does: the envelope has
/// to be collected *after* the commit or its commit count and diff stat are
/// stale, but a caller that hangs up mid-task never asks for an envelope and
/// its worker's partial edits still have to be durable.
pub async fn create_with_commit_hook(
    workspace_root: &str,
    worker: &str,
    verification: Option<String>,
) -> Result<Option<(PathBuf, EvidenceHook, ExitHook)>> {
    let Some(tree) = create(workspace_root, worker).await? else {
        return Ok(None);
    };
    let cwd = tree.path.clone();
    let tree = Arc::new(tree);
    let committed = Arc::new(AtomicBool::new(false));

    let evidence: EvidenceHook = {
        let tree = tree.clone();
        let committed = committed.clone();
        Box::new(move || {
            Box::pin(async move {
                commit_once(&tree, &committed).await;
                collect(&tree, verification.as_deref()).await
            })
        })
    };
    let on_exit: ExitHook = Box::new(move |_succeeded| {
        // `on_exit` runs from the runner's `Drop`, which cannot await;
        // committing is a `git` subprocess, so it is spawned. The tree is
        // the worker's alone, so nothing races this.
        tokio::spawn(async move {
            // Committed whether the worker succeeded or not: a failed
            // worker's partial edits are still the only copy that exists.
            commit_once(&tree, &committed).await;
        });
    });
    Ok(Some((cwd, evidence, on_exit)))
}

/// [`commit`], unless something already did.
async fn commit_once(tree: &WorkerTree, committed: &AtomicBool) {
    if committed.swap(true, Ordering::SeqCst) {
        return;
    }
    commit(tree, "delegated task").await;
}

/// What the worker's tree says once its output is committed.
///
/// `None` when there is nothing to report: no default branch to measure
/// against, or — the case the issue is about — a branch with no commits on
/// it. A read-only reviewer must not be handed a branch to merge.
pub async fn collect(tree: &WorkerTree, verification: Option<&str>) -> Option<Evidence> {
    let path = tree.path.to_string_lossy().to_string();
    let git = match GitService::new(&path).await {
        Ok(git) => git,
        Err(e) => {
            warn!(worktree = %tree.name, error = %e, "Cannot open the worker's worktree to collect its evidence");
            return None;
        }
    };

    let base = git.default_branch().await?;
    let commits = git.commits_ahead(&base, &tree.branch).await.unwrap_or(0);
    if commits == 0 {
        info!(branch = %tree.branch, "Worker left no commits; no evidence envelope");
        return None;
    }
    let diff_stat = git
        .diff_stat(&base, &tree.branch)
        .await
        .unwrap_or_else(|e| format!("(diff --stat failed: {e})"));

    let verification = match verification {
        Some(command) => run_verification(&tree.path, command, VERIFICATION_TIMEOUT).await,
        None => None,
    };

    Some(Evidence {
        branch: tree.branch.clone(),
        base,
        commits,
        diff_stat,
        verification,
    })
}

/// Run the team's verification command in the worker's tree.
///
/// A plain subprocess rather than the agent's sandboxed shell tool: this is
/// the *runner's* check on a worker's claim, so running it through anything
/// the worker could influence would defeat the point.
///
/// `timeout` is [`VERIFICATION_TIMEOUT`] in production; a parameter only so
/// the timeout path is testable in under five minutes.
async fn run_verification(
    cwd: &std::path::Path,
    command: &str,
    timeout: Duration,
) -> Option<Verification> {
    let mut cmd = shell_command(command);
    cmd.current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    // A verification command is a test suite, so the `sh` the runner starts
    // is never the process that does the work. `kill_on_drop` reaches only
    // that shell, which would leave the suite itself running in the
    // worker's tree long after the delegation reported a timeout — so the
    // command gets a process group of its own and the timeout kills the
    // whole group.
    #[cfg(unix)]
    cmd.process_group(0);

    let started = tokio::time::Instant::now();
    let child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            return Some(Verification {
                command: command.to_string(),
                exit_code: None,
                timed_out: false,
                output: format!("the command could not be run: {e}"),
            });
        }
    };
    // Read before the child is consumed by `wait_with_output`, which is
    // what has to be killed if the wait is abandoned.
    let group = child.id().map(|id| id as i32);

    let finished = tokio::time::timeout(timeout, child.wait_with_output()).await;
    let (exit_code, timed_out, output) = match finished {
        Ok(Ok(out)) => {
            let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
            text.push_str(&String::from_utf8_lossy(&out.stderr));
            (out.status.code(), false, text)
        }
        Ok(Err(e)) => (None, false, format!("the command could not be run: {e}")),
        Err(_) => {
            kill_process_group(group);
            (None, true, String::new())
        }
    };
    info!(
        command,
        ?exit_code,
        timed_out,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "Ran the team's verification command in a worker's worktree"
    );

    Some(Verification {
        command: command.to_string(),
        exit_code,
        timed_out,
        output: tail_lines(&output, VERIFICATION_TAIL_LINES),
    })
}

/// SIGKILL every process the verification command started.
///
/// The group leader is the `sh` the runner spawned, and by the time this
/// runs the timeout has already dropped it — but the kernel keeps a pid
/// reserved for as long as it names a process group with live members, so
/// the number cannot have been recycled underneath us. `ESRCH` is the
/// ordinary case of a command that had already finished forking: nothing
/// is left to kill.
#[cfg(unix)]
fn kill_process_group(group: Option<i32>) {
    use nix::errno::Errno;
    use nix::sys::signal::{Signal, killpg};
    use nix::unistd::Pid;

    let Some(group) = group else { return };
    match killpg(Pid::from_raw(group), Signal::SIGKILL) {
        Ok(()) => info!(
            group,
            "Killed the verification command's process group on timeout"
        ),
        Err(Errno::ESRCH) => {}
        Err(e) => {
            warn!(group, error = %e, "Could not kill the verification command's process group")
        }
    }
}

/// Windows has no process groups to signal; the command is left to
/// `kill_on_drop`, as it was before AGE-406.
#[cfg(not(unix))]
fn kill_process_group(_group: Option<i32>) {}

/// The platform shell, so a declared command is written the way the team
/// would type it rather than as an argv.
fn shell_command(command: &str) -> tokio::process::Command {
    #[cfg(windows)]
    {
        let mut cmd = tokio::process::Command::new("cmd");
        cmd.arg("/C").arg(command);
        cmd
    }
    #[cfg(not(windows))]
    {
        let mut cmd = tokio::process::Command::new("sh");
        cmd.arg("-c").arg(command);
        cmd
    }
}

/// The last `n` non-empty-trailing lines of `text`.
fn tail_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.trim_end().lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
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

    /// A repository with one commit, so worktrees can be added. `-b main`
    /// so the default branch does not depend on the machine's
    /// `init.defaultBranch`.
    async fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        git(&["init", "-q", "-b", "main"], dir.path()).await;
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

    // -----------------------------------------------------------------
    // The evidence envelope (ADR-0011 C12, AGE-406)
    // -----------------------------------------------------------------

    /// Write `file` into `tree` and commit it, standing in for a worker
    /// whose output was committed on the way out.
    async fn commit_a_change(tree: &WorkerTree, file: &str, body: &str) {
        std::fs::write(tree.path.join(file), body).unwrap();
        commit(tree, "delegated task").await;
    }

    /// Do item 1: branch, diff stat, commit count and the verification
    /// command's exit code and tail, all read off the tree by the runner.
    #[tokio::test]
    async fn a_worker_that_committed_gets_an_envelope_with_its_diff_and_verification() {
        let dir = repo().await;
        let tree = create(&dir.path().to_string_lossy(), "local-coder-0")
            .await
            .unwrap()
            .unwrap();
        commit_a_change(&tree, "added.rs", "fn main() {}\n").await;

        let evidence = collect(&tree, Some("echo checked; exit 3"))
            .await
            .expect("a branch with a commit on it has evidence");

        assert_eq!(evidence.branch, "sub-agent/local-coder-0");
        assert_eq!(evidence.base, "main");
        assert_eq!(evidence.commits, 1);
        assert!(evidence.diff_stat.contains("added.rs"), "{evidence:?}");
        let check = evidence.verification.as_ref().expect("it was declared");
        assert_eq!(check.exit_code, Some(3));
        assert!(!check.timed_out);
        assert_eq!(check.output, "checked");

        let block = evidence.block();
        assert!(
            block.starts_with("\n\n```evidence\n") && block.ends_with("```"),
            "{block}"
        );
        assert!(block.contains("branch: sub-agent/local-coder-0"), "{block}");
        assert!(block.contains("added.rs"), "{block}");
        assert!(block.contains("exit code 3"), "{block}");
        assert_eq!(evidence.json()["commits"], 1);
    }

    /// Do item 2: a reviewer that committed nothing gets no envelope, so it
    /// is never handed a branch to merge.
    #[tokio::test]
    async fn a_worker_that_committed_nothing_gets_no_envelope() {
        let dir = repo().await;
        let tree = create(&dir.path().to_string_lossy(), "local-reviewer-0")
            .await
            .unwrap()
            .unwrap();
        commit(&tree, "delegated task").await;

        assert!(
            collect(&tree, Some("exit 0")).await.is_none(),
            "an empty branch is not evidence of anything"
        );
    }

    /// `master` is as much a default branch as `main` is.
    #[tokio::test]
    async fn the_default_branch_may_be_master() {
        let dir = tempfile::tempdir().unwrap();
        git(&["init", "-q", "-b", "master"], dir.path()).await;
        git(&["config", "user.email", "t@example.com"], dir.path()).await;
        git(&["config", "user.name", "T"], dir.path()).await;
        std::fs::write(dir.path().join("README"), "hi").unwrap();
        git(&["add", "."], dir.path()).await;
        git(&["commit", "-q", "-m", "init"], dir.path()).await;

        let tree = create(&dir.path().to_string_lossy(), "local-coder-0")
            .await
            .unwrap()
            .unwrap();
        commit_a_change(&tree, "added.rs", "fn main() {}\n").await;

        let evidence = collect(&tree, None).await.expect("there is a commit");
        assert_eq!(evidence.base, "master");
        assert!(evidence.verification.is_none(), "none was declared");
        assert!(!evidence.block().contains("verification:"));
    }

    /// The commit has to have happened before the envelope is read, or the
    /// count and the diff stat describe the tree as it was one edit ago.
    #[tokio::test]
    async fn the_hook_commits_before_it_collects_and_the_exit_hook_does_not_commit_twice() {
        let dir = repo().await;
        let root = dir.path().to_string_lossy().to_string();
        let (cwd, evidence, on_exit) = create_with_commit_hook(&root, "local-coder-0", None)
            .await
            .unwrap()
            .unwrap();
        // Uncommitted, exactly as a worker leaves its tree.
        std::fs::write(cwd.join("added.rs"), "fn main() {}\n").unwrap();

        let evidence = evidence().await.expect("the hook commits, then measures");
        assert_eq!(evidence.commits, 1);
        assert!(evidence.diff_stat.contains("added.rs"), "{evidence:?}");

        std::fs::write(cwd.join("later.rs"), "// after the envelope\n").unwrap();
        on_exit(true);
        tokio::time::sleep(Duration::from_millis(200)).await;
        let after = collect(
            &WorkerTree {
                name: "local-coder-0".into(),
                branch: "sub-agent/local-coder-0".into(),
                path: cwd,
            },
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            after.commits, 1,
            "the envelope already committed, so the exit hook has nothing left to do"
        );
    }

    /// A command that never returns is killed and reported as timed out,
    /// rather than holding the delegation open — and the kill reaches what
    /// the command *started*, not only the `sh` the runner spawned.
    ///
    /// The probe is a backgrounded child that outlives its shell: with only
    /// `kill_on_drop` it survives the timeout and leaves `survivor` behind,
    /// and a `sleep` with a duration nothing else on this machine uses is
    /// still in the process table. Both are checked, because the file says
    /// the child *ran on* and `pgrep` says it is *still running*.
    #[tokio::test]
    async fn a_hanging_verification_command_times_out_and_takes_its_children_with_it() {
        let dir = tempfile::tempdir().unwrap();
        let survivor = dir.path().join("survivor");
        let marker = format!("864.{}", std::process::id() % 1000);
        let script = format!("(sleep 0.4; : > survivor) & sleep {marker} & wait");

        let outcome = run_verification(dir.path(), &script, Duration::from_millis(150))
            .await
            .expect("a declared command always reports something");

        assert!(outcome.timed_out);
        assert_eq!(outcome.exit_code, None);

        tokio::time::sleep(Duration::from_millis(900)).await;
        assert!(
            !survivor.exists(),
            "the timeout killed only the shell; the command's own children ran on"
        );
        if let Ok(found) = std::process::Command::new("pgrep")
            .arg("-f")
            .arg(format!("sleep {marker}"))
            .output()
        {
            assert!(
                !found.status.success(),
                "a `sleep {marker}` from the verification command survived its timeout: {}",
                String::from_utf8_lossy(&found.stdout)
            );
        }
        assert!(
            Evidence {
                branch: "sub-agent/w1".into(),
                base: "main".into(),
                commits: 1,
                diff_stat: String::new(),
                verification: Some(outcome),
            }
            .block()
            .contains("timed out")
        );
    }

    #[test]
    fn only_the_last_lines_of_a_long_verification_run_are_kept() {
        let long: String = (1..=50).map(|i| format!("line {i}\n")).collect();
        let tail = tail_lines(&long, VERIFICATION_TAIL_LINES);
        assert_eq!(tail.lines().count(), VERIFICATION_TAIL_LINES);
        assert!(tail.starts_with("line 31"), "{tail}");
        assert!(tail.ends_with("line 50"), "{tail}");
    }
}
