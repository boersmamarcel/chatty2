use anyhow::{Result, anyhow};
use serde::Serialize;
use std::path::PathBuf;
use tracing::{debug, info};

use super::path_validator::PathValidator;

/// Output from `git status`
#[derive(Debug, Serialize)]
pub struct GitStatusOutput {
    /// Current branch name
    pub branch: String,
    /// Staged files
    pub staged: Vec<String>,
    /// Modified (unstaged) files
    pub modified: Vec<String>,
    /// Untracked files
    pub untracked: Vec<String>,
    /// Raw status output
    pub raw: String,
}

/// A single entry from `git log`
#[derive(Debug, Serialize)]
pub struct GitLogEntry {
    pub hash: String,
    pub author: String,
    pub date: String,
    pub message: String,
}

/// Output from `git add`
#[derive(Debug, Serialize)]
pub struct GitAddOutput {
    /// Files that were staged
    pub staged_files: Vec<String>,
    /// Summary message
    pub message: String,
}

/// Output from `git commit`
#[derive(Debug, Serialize)]
pub struct GitCommitOutput {
    /// The commit hash
    pub hash: String,
    /// The commit message used
    pub message: String,
    /// Summary line from git (e.g., "1 file changed, 2 insertions(+)")
    pub summary: String,
}

/// A single entry from `git worktree list`
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct GitWorktree {
    /// Absolute path of the worktree's working directory
    pub path: String,
    /// Branch checked out there, if any (detached worktrees have none)
    pub branch: Option<String>,
}

/// Directory, relative to the workspace root, that holds per-worker worktrees.
///
/// ADR-0012 leaves the location open between "under the workspace" and "beside
/// it"; under it keeps every worker path inside the `PathValidator` root, which
/// is the property the filesystem tools rely on.
pub const WORKTREE_DIR: &str = ".chatty/worktrees";

/// Git operations service.
///
/// All operations are workspace-restricted via PathValidator and executed
/// using `tokio::process::Command`. Dangerous operations (force push,
/// hard reset) are intentionally excluded.
pub struct GitService {
    workspace_root: PathBuf,
    validator: PathValidator,
}

impl std::fmt::Debug for GitService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitService")
            .field("workspace_root", &self.workspace_root)
            .finish()
    }
}

impl GitService {
    /// Create a new GitService with the given workspace root.
    ///
    /// Validates the workspace exists and is a git repository.
    pub async fn new(workspace_root: &str) -> Result<Self> {
        let validator = PathValidator::new(workspace_root).await?;
        let root = validator.workspace_root().to_path_buf();

        // Verify git is installed
        let git_check = tokio::process::Command::new("git")
            .arg("--version")
            .output()
            .await
            .map_err(|e| anyhow!("Git is not installed or not in PATH: {}", e))?;

        if !git_check.status.success() {
            return Err(anyhow!("Git is not available on this system"));
        }

        // Verify workspace is a git repository
        let repo_check = tokio::process::Command::new("git")
            .args(["rev-parse", "--git-dir"])
            .current_dir(&root)
            .output()
            .await
            .map_err(|e| anyhow!("Failed to check git repository: {}", e))?;

        if !repo_check.status.success() {
            return Err(anyhow!(
                "'{}' is not a git repository. Initialize with 'git init' first.",
                workspace_root
            ));
        }

        info!(workspace = %root.display(), "Git service initialized");

        Ok(Self {
            workspace_root: root,
            validator,
        })
    }

    /// Run a git command in the workspace directory.
    async fn run_git(&self, args: &[&str]) -> Result<String> {
        debug!(args = ?args, "Running git command");

        let output = tokio::process::Command::new("git")
            .args(args)
            .current_dir(&self.workspace_root)
            .output()
            .await
            .map_err(|e| anyhow!("Failed to execute git command: {}", e))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow!("git {} failed: {}", args.join(" "), stderr.trim()));
        }

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        Ok(stdout)
    }

    /// Get repository status.
    pub async fn status(&self) -> Result<GitStatusOutput> {
        // Get branch name
        let branch = self
            .run_git(&["branch", "--show-current"])
            .await
            .unwrap_or_else(|_| "HEAD (detached)".to_string())
            .trim()
            .to_string();

        // Get porcelain status for parsing
        let raw = self.run_git(&["status", "--porcelain=v1"]).await?;

        let mut staged = Vec::new();
        let mut modified = Vec::new();
        let mut untracked = Vec::new();

        for line in raw.lines() {
            if line.len() < 3 {
                continue;
            }
            let index_status = line.as_bytes()[0] as char;
            let worktree_status = line.as_bytes()[1] as char;
            let file = line[3..].to_string();

            // Staged changes (index column)
            if matches!(index_status, 'A' | 'M' | 'D' | 'R' | 'C') {
                staged.push(format!("{} {}", index_status, file));
            }

            // Unstaged changes (worktree column)
            if matches!(worktree_status, 'M' | 'D') {
                modified.push(format!("{} {}", worktree_status, file));
            }

            // Untracked
            if index_status == '?' {
                untracked.push(file);
            }
        }

        // Get human-readable status for raw output
        let human_status = self.run_git(&["status"]).await?;

        Ok(GitStatusOutput {
            branch,
            staged,
            modified,
            untracked,
            raw: human_status.trim().to_string(),
        })
    }

    /// Get diff output.
    ///
    /// If `staged` is true, shows staged changes (`git diff --cached`).
    /// Otherwise shows unstaged changes.
    /// If `path` is provided, it is validated to be within the workspace
    /// boundary before being passed to git.
    pub async fn diff(&self, staged: bool, path: Option<&str>) -> Result<String> {
        // Validate path is within workspace if provided
        let validated_path: Option<String> = match path {
            Some(p) => {
                // Use validate_parent which handles both existing and non-existing paths
                // (a file may be deleted but still show in diff)
                let _ = self.validator.validate_parent(p).await.map_err(|e| {
                    anyhow!("Path '{}' is outside the workspace or invalid: {}", p, e)
                })?;
                Some(p.to_string())
            }
            None => None,
        };

        let mut args = vec!["diff"];
        if staged {
            args.push("--cached");
        }
        if let Some(ref p) = validated_path {
            args.push("--");
            args.push(p);
        }

        let output = self.run_git(&args).await?;
        if output.trim().is_empty() {
            Ok("No changes found.".to_string())
        } else {
            Ok(output)
        }
    }

    /// Get commit log.
    ///
    /// Returns up to `max_count` recent commits.
    pub async fn log(&self, max_count: u32) -> Result<Vec<GitLogEntry>> {
        let count_str = max_count.to_string();
        let output = self
            .run_git(&[
                "log",
                &format!("--max-count={}", count_str),
                "--format=%H%n%an%n%ai%n%s%n---END---",
            ])
            .await?;

        let mut entries = Vec::new();
        let mut lines = output.lines();

        loop {
            let hash = match lines.next() {
                Some(h) if !h.is_empty() => h.to_string(),
                _ => break,
            };
            let author = lines.next().unwrap_or("").to_string();
            let date = lines.next().unwrap_or("").to_string();
            let message = lines.next().unwrap_or("").to_string();
            // Consume the ---END--- separator
            let _ = lines.next();

            entries.push(GitLogEntry {
                hash,
                author,
                date,
                message,
            });
        }

        Ok(entries)
    }

    /// Stage files for commit.
    ///
    /// Each path is validated to be within the workspace. Supports individual
    /// files and directories. Does NOT accept the "." shorthand — callers
    /// must enumerate paths explicitly to prevent accidentally staging
    /// secrets or large binaries.
    pub async fn add(&self, paths: &[String]) -> Result<GitAddOutput> {
        if paths.is_empty() {
            return Err(anyhow!("At least one path is required"));
        }

        // Validate every path is within the workspace.
        // Uses PathValidator::validate() which canonicalizes both the path and
        // the workspace root, so symlinked roots (e.g. /tmp → /private/tmp on
        // macOS) are handled correctly.
        for p in paths {
            self.validator.validate(p).await?;
        }

        let path_refs: Vec<&str> = paths.iter().map(|s| s.as_str()).collect();
        let mut args = vec!["add", "--"];
        args.extend(path_refs.iter());

        self.run_git(&args).await?;

        info!(paths = ?paths, "Files staged");

        Ok(GitAddOutput {
            staged_files: paths.to_vec(),
            message: format!("Successfully staged {} file(s)", paths.len()),
        })
    }

    /// Create a new branch.
    pub async fn create_branch(&self, name: &str) -> Result<String> {
        // Validate branch name doesn't contain dangerous characters
        Self::validate_branch_name(name)?;

        self.run_git(&["branch", name]).await?;

        info!(branch = %name, "Branch created");
        Ok(format!("Branch '{}' created successfully", name))
    }

    /// Switch to an existing branch.
    pub async fn switch_branch(&self, name: &str) -> Result<String> {
        Self::validate_branch_name(name)?;

        self.run_git(&["switch", name]).await?;

        info!(branch = %name, "Switched to branch");
        Ok(format!("Switched to branch '{}'", name))
    }

    /// Create a worktree for one parallel worker, on its own new branch.
    ///
    /// ADR-0012: a worker gets its own copy and hands back a branch, so two
    /// workers editing one file produce a git conflict the leader can see
    /// rather than a last-writer-wins result neither of them reported.
    ///
    /// `name` is a single path segment; the worktree lands at
    /// `<workspace>/.chatty/worktrees/<name>` and the branch is created there.
    pub async fn worktree_add(&self, name: &str, branch: &str) -> Result<PathBuf> {
        Self::validate_worktree_name(name)?;
        Self::validate_branch_name(branch)?;

        self.exclude_worktree_dir_locally().await?;

        let rel = format!("{WORKTREE_DIR}/{name}");
        let path = self.workspace_root.join(&rel);
        if path.exists() {
            return Err(anyhow!(
                "Worktree '{}' already exists at {}",
                name,
                path.display()
            ));
        }

        self.run_git(&["worktree", "add", "-b", branch, &rel])
            .await?;

        info!(worktree = %path.display(), branch = %branch, "Worktree created");
        Ok(path)
    }

    /// Remove a worker's worktree. The branch is kept — it is the worker's
    /// output, and the leader still has to merge it.
    ///
    /// `force` is off by default on purpose. `git worktree remove` refuses a
    /// tree with modified or untracked files, and that refusal is the only
    /// guard against destroying work no commit has captured yet. Removal is a
    /// separate step *after* a successful merge, never part of a worker's exit
    /// path; see [`Self::commit_all`].
    pub async fn worktree_remove(&self, name: &str, force: bool) -> Result<String> {
        Self::validate_worktree_name(name)?;

        let rel = format!("{WORKTREE_DIR}/{name}");
        let mut args = vec!["worktree", "remove"];
        if force {
            args.push("--force");
        }
        args.push(&rel);
        self.run_git(&args).await?;

        info!(worktree = %name, force, "Worktree removed");
        Ok(format!("Worktree '{}' removed", name))
    }

    /// Keep the harness's own worktrees out of the user's repository.
    ///
    /// [`WORKTREE_DIR`] sits inside the workspace so every worker path stays
    /// within the `PathValidator` root (ADR-0017). The cost is borne by the
    /// user's repo: `git status` reports `?? .chatty/`, and `git add -A`
    /// stages a worktree as an *embedded git repository*, dropping a gitlink
    /// into their history. `commit_all` is itself a `git add -A`, so this is
    /// reachable from the harness and not only from the user.
    ///
    /// `.git/info/exclude` is the right lever: it is git's per-clone ignore
    /// list, so the fix never touches the `.gitignore` the user tracks and
    /// owns. Written to the *common* git dir, which linked worktrees share.
    async fn exclude_worktree_dir_locally(&self) -> Result<()> {
        let entry = format!("/{}/", WORKTREE_DIR.trim_end_matches('/'));

        let common = self.run_git(&["rev-parse", "--git-common-dir"]).await?;
        let common = common.trim();
        let git_dir = if std::path::Path::new(common).is_absolute() {
            PathBuf::from(common)
        } else {
            self.workspace_root.join(common)
        };

        let info_dir = git_dir.join("info");
        let exclude = info_dir.join("exclude");

        let current = tokio::fs::read_to_string(&exclude)
            .await
            .unwrap_or_default();
        if current.lines().any(|line| line.trim() == entry) {
            return Ok(());
        }

        tokio::fs::create_dir_all(&info_dir)
            .await
            .map_err(|e| anyhow!("Failed to create {}: {}", info_dir.display(), e))?;

        let mut next = current;
        if !next.is_empty() && !next.ends_with('\n') {
            next.push('\n');
        }
        next.push_str("# chatty sub-agent worktrees (ADR-0017); local-only, not your .gitignore\n");
        next.push_str(&entry);
        next.push('\n');

        tokio::fs::write(&exclude, next)
            .await
            .map_err(|e| anyhow!("Failed to write {}: {}", exclude.display(), e))?;

        debug!(exclude = %exclude.display(), entry = %entry, "Excluded worktree dir locally");
        Ok(())
    }

    /// Stage everything and commit it, returning `None` when the tree is clean.
    ///
    /// This is the local form of ADR-0016's turn-commit barrier: a worker's
    /// output is durable on its branch before anything else may remove the
    /// tree it lives in.
    ///
    /// `git add -A` honours `.gitignore`, so build state stays out. It does
    /// *not* separate generated artifacts from tracked source — an un-ignored
    /// PDF lands in git history, which ADR-0016 rejects. Classifying it is
    /// that ADR's open question; committing everything is the choice that
    /// cannot lose data while the question is open.
    pub async fn commit_all(&self, message: &str) -> Result<Option<GitCommitOutput>> {
        if message.trim().is_empty() {
            return Err(anyhow!("Commit message cannot be empty"));
        }

        self.run_git(&["add", "-A"]).await?;

        let staged = self.run_git(&["diff", "--cached", "--stat"]).await?;
        if staged.trim().is_empty() {
            debug!("Nothing to commit; worktree is clean");
            return Ok(None);
        }

        let output = self.run_git(&["commit", "-m", message]).await?;
        let hash = self
            .run_git(&["rev-parse", "HEAD"])
            .await?
            .trim()
            .to_string();

        info!(hash = %hash, "Worker output committed");

        Ok(Some(GitCommitOutput {
            hash,
            message: message.to_string(),
            summary: output.trim().to_string(),
        }))
    }

    /// List the repository's worktrees, including the main one.
    pub async fn worktree_list(&self) -> Result<Vec<GitWorktree>> {
        let output = self.run_git(&["worktree", "list", "--porcelain"]).await?;
        Ok(Self::parse_worktree_list(&output))
    }

    /// Parse `git worktree list --porcelain` into entries.
    ///
    /// Records are separated by a blank line; `worktree <path>` opens one and
    /// `branch refs/heads/<name>` names its branch. A detached worktree has a
    /// `detached` line and no branch.
    fn parse_worktree_list(output: &str) -> Vec<GitWorktree> {
        let mut out = Vec::new();
        let mut path: Option<String> = None;
        let mut branch: Option<String> = None;

        for line in output.lines() {
            if let Some(rest) = line.strip_prefix("worktree ") {
                if let Some(p) = path.take() {
                    out.push(GitWorktree {
                        path: p,
                        branch: branch.take(),
                    });
                }
                path = Some(rest.trim().to_string());
            } else if let Some(rest) = line.strip_prefix("branch ") {
                branch = Some(
                    rest.trim()
                        .strip_prefix("refs/heads/")
                        .unwrap_or(rest.trim())
                        .to_string(),
                );
            }
        }
        if let Some(p) = path {
            out.push(GitWorktree { path: p, branch });
        }
        out
    }

    /// Validate a worktree name: one path segment, no traversal.
    ///
    /// The name reaches `git worktree add` as a path relative to the workspace
    /// root, so anything that could climb out of `WORKTREE_DIR` has to be
    /// rejected here rather than caught later by `PathValidator`.
    fn validate_worktree_name(name: &str) -> Result<()> {
        if name.is_empty() {
            return Err(anyhow!("Worktree name cannot be empty"));
        }
        if name.len() > 100 {
            return Err(anyhow!("Worktree name too long (max 100 characters)"));
        }
        if name.contains('/') || name.contains('\\') {
            return Err(anyhow!("Worktree name must be a single path segment"));
        }
        if name == "." || name == ".." || name.contains("..") {
            return Err(anyhow!("Worktree name cannot contain '..'"));
        }
        if name.starts_with('-') {
            return Err(anyhow!("Worktree name cannot start with '-'"));
        }
        if !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
        {
            return Err(anyhow!(
                "Worktree name may only contain letters, digits, '-', '_' and '.'"
            ));
        }
        Ok(())
    }

    /// Create a commit with the given message.
    ///
    /// Only commits already-staged changes. Returns an error if there are
    /// no staged changes.
    pub async fn commit(&self, message: &str) -> Result<GitCommitOutput> {
        if message.trim().is_empty() {
            return Err(anyhow!("Commit message cannot be empty"));
        }

        // Check there are staged changes
        let staged_check = self.run_git(&["diff", "--cached", "--stat"]).await?;
        if staged_check.trim().is_empty() {
            return Err(anyhow!(
                "No staged changes to commit. Use 'git add' to stage changes first."
            ));
        }

        // Perform the commit
        let output = self.run_git(&["commit", "-m", message]).await?;

        // Get the commit hash
        let hash = self
            .run_git(&["rev-parse", "HEAD"])
            .await?
            .trim()
            .to_string();

        info!(hash = %hash, "Commit created");

        Ok(GitCommitOutput {
            hash,
            message: message.to_string(),
            summary: output.trim().to_string(),
        })
    }

    /// Validate a branch name per `git check-ref-format` rules.
    ///
    /// See <https://git-scm.com/docs/git-check-ref-format> for the full spec.
    fn validate_branch_name(name: &str) -> Result<()> {
        if name.is_empty() {
            return Err(anyhow!("Branch name cannot be empty"));
        }
        if name.len() > 255 {
            return Err(anyhow!("Branch name too long (max 255 characters)"));
        }

        // Forbidden characters: git-reserved + shell metacharacters + glob chars
        let forbidden = [
            '~', '^', ':', '\\', ' ', '\t', '\n', '\x7f', // original set
            '?', '*', '[', // glob characters rejected by git
        ];
        for ch in &forbidden {
            if name.contains(*ch) {
                return Err(anyhow!(
                    "Branch name contains invalid character: '{}'",
                    ch.escape_default()
                ));
            }
        }
        // No ASCII control characters (0x00–0x1F, 0x7F)
        if name.bytes().any(|b| b < 0x20 || b == 0x7f) {
            return Err(anyhow!("Branch name cannot contain control characters"));
        }

        // Sequence rules
        if name.contains("..") {
            return Err(anyhow!("Branch name cannot contain '..'"));
        }
        if name.contains("@{") {
            return Err(anyhow!("Branch name cannot contain '@{{'"));
        }
        if name.contains("//") {
            return Err(anyhow!("Branch name cannot contain consecutive slashes"));
        }

        // Start/end rules
        if name.starts_with('-') {
            return Err(anyhow!("Branch name cannot start with '-'"));
        }
        if name.starts_with('/') || name.ends_with('/') {
            return Err(anyhow!("Branch name cannot start or end with '/'"));
        }
        if name.ends_with('.') {
            return Err(anyhow!("Branch name cannot end with '.'"));
        }
        if name.ends_with(".lock") {
            return Err(anyhow!("Branch name cannot end with '.lock'"));
        }

        // No path component can start with '.' (e.g. "feat/.hidden" is invalid)
        for component in name.split('/') {
            if component.starts_with('.') {
                return Err(anyhow!(
                    "Branch name component cannot start with '.': '{}'",
                    component
                ));
            }
            if component.is_empty() {
                // Already caught by the "//" check, but be defensive
                return Err(anyhow!("Branch name cannot contain empty path components"));
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Helper to create a temporary git repository
    // ── Worktree isolation (AGE-314 / ADR-0012) ──────────────────────────

    #[test]
    fn worktree_name_rejects_traversal_and_flags() {
        for bad in ["", "..", "../evil", "a/b", "a..b", "-rf", "wt$(x)", "."] {
            assert!(
                GitService::validate_worktree_name(bad).is_err(),
                "should reject {bad:?}"
            );
        }
        for good in ["w1", "w-1700000000-0", "worker_2", "a.b"] {
            assert!(
                GitService::validate_worktree_name(good).is_ok(),
                "should accept {good:?}"
            );
        }
    }

    #[test]
    fn parses_worktree_list_porcelain() {
        let out = "worktree /repo\nHEAD abc\nbranch refs/heads/main\n\n\
                   worktree /repo/.chatty/worktrees/w1\nHEAD def\nbranch refs/heads/sub-agent/w1\n\n\
                   worktree /repo/.chatty/worktrees/w2\nHEAD 123\ndetached\n";
        let got = GitService::parse_worktree_list(out);
        assert_eq!(got.len(), 3);
        assert_eq!(got[0].branch.as_deref(), Some("main"));
        assert_eq!(got[1].path, "/repo/.chatty/worktrees/w1");
        assert_eq!(got[1].branch.as_deref(), Some("sub-agent/w1"));
        assert_eq!(got[2].branch, None, "a detached worktree has no branch");
    }

    #[tokio::test]
    async fn worktree_add_creates_an_isolated_checkout() {
        let (tmp, git) = create_test_repo().await;
        let path = git.worktree_add("w1", "sub-agent/w1").await.unwrap();

        assert!(path.exists(), "worktree directory should exist");
        assert!(
            path.starts_with(tmp.path()),
            "worktree stays inside the workspace root"
        );

        let listed = git.worktree_list().await.unwrap();
        assert!(
            listed
                .iter()
                .any(|w| w.branch.as_deref() == Some("sub-agent/w1")),
            "new worktree should be listed: {listed:?}"
        );
    }

    /// A worker's worktree lives inside the user's repository, so it must be
    /// invisible to the user's own git. Without the local exclude, `git status`
    /// reports `?? .chatty/` and `git add -A` stages the worktree as an
    /// embedded repository — a gitlink in their history.
    #[tokio::test]
    async fn worktree_add_leaves_the_parent_repo_status_clean() {
        let (_tmp, git) = create_test_repo().await;
        git.worktree_add("w1", "sub-agent/w1").await.unwrap();

        let status = git.run_git(&["status", "--porcelain"]).await.unwrap();
        assert!(
            status.trim().is_empty(),
            "worktrees must not show up in the user's status, got: {status:?}"
        );

        git.run_git(&["add", "-A"]).await.unwrap();
        let staged = git
            .run_git(&["diff", "--cached", "--name-only"])
            .await
            .unwrap();
        assert!(
            staged.trim().is_empty(),
            "`git add -A` must not stage the worktree, got: {staged:?}"
        );
    }

    /// The property the whole teardown design rests on: a worker's uncommitted
    /// work is never destroyed as a side effect of cleanup.
    #[tokio::test]
    async fn worktree_remove_refuses_to_discard_uncommitted_work() {
        let (_tmp, git) = create_test_repo().await;
        let path = git.worktree_add("w1", "sub-agent/w1").await.unwrap();

        tokio::fs::write(path.join("worker.txt"), "unsaved work")
            .await
            .unwrap();

        let refused = git.worktree_remove("w1", false).await;
        assert!(refused.is_err(), "must not delete a dirty worktree");
        assert!(
            path.join("worker.txt").exists(),
            "the worker's file must survive the refused removal"
        );

        git.worktree_remove("w1", true)
            .await
            .expect("force removal is the explicit opt-in");
    }

    #[tokio::test]
    async fn commit_all_captures_worker_output_then_removal_succeeds() {
        let (_tmp, git) = create_test_repo().await;
        let path = git.worktree_add("w1", "sub-agent/w1").await.unwrap();

        tokio::fs::write(path.join("worker.txt"), "the answer")
            .await
            .unwrap();

        let worker_git = GitService::new(path.to_str().unwrap()).await.unwrap();
        let commit = worker_git
            .commit_all("sub-agent w1: do the task")
            .await
            .unwrap()
            .expect("a dirty worktree should produce a commit");
        assert!(!commit.hash.is_empty());

        // Committed, so the non-forced removal now succeeds.
        git.worktree_remove("w1", false)
            .await
            .expect("a clean worktree removes without force");

        // And the output survives on the branch.
        let log = git
            .run_git(&["log", "--oneline", "sub-agent/w1"])
            .await
            .unwrap();
        assert!(
            log.contains("sub-agent w1"),
            "branch keeps the output: {log}"
        );
    }

    #[tokio::test]
    async fn commit_all_on_a_clean_tree_is_none() {
        let (_tmp, git) = create_test_repo().await;
        let path = git.worktree_add("w1", "sub-agent/w1").await.unwrap();
        let worker_git = GitService::new(path.to_str().unwrap()).await.unwrap();
        assert!(
            worker_git.commit_all("nothing").await.unwrap().is_none(),
            "a worker that changed nothing produces no commit"
        );
    }

    async fn create_test_repo() -> (tempfile::TempDir, GitService) {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().to_str().unwrap();

        // Initialize git repo
        tokio::process::Command::new("git")
            .args(["init"])
            .current_dir(path)
            .output()
            .await
            .unwrap();

        // Configure git user for commits
        tokio::process::Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(path)
            .output()
            .await
            .unwrap();
        tokio::process::Command::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(path)
            .output()
            .await
            .unwrap();

        // Disable commit signing so tests work in environments with
        // global gpgsign enabled (e.g. CI with signing servers).
        tokio::process::Command::new("git")
            .args(["config", "commit.gpgsign", "false"])
            .current_dir(path)
            .output()
            .await
            .unwrap();

        let service = GitService::new(path).await.unwrap();
        (tmp, service)
    }

    #[tokio::test]
    async fn test_new_not_a_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let result = GitService::new(tmp.path().to_str().unwrap()).await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("not a git repository")
        );
    }

    #[tokio::test]
    async fn test_status_empty_repo() {
        let (tmp, service) = create_test_repo().await;

        // Create a file so we have something to track
        fs::write(tmp.path().join("test.txt"), "hello").unwrap();

        let status = service.status().await.unwrap();
        assert!(!status.branch.is_empty());
        assert!(status.untracked.iter().any(|f| f.contains("test.txt")));
    }

    #[tokio::test]
    async fn test_status_staged_file() {
        let (tmp, service) = create_test_repo().await;

        fs::write(tmp.path().join("staged.txt"), "content").unwrap();
        service.run_git(&["add", "staged.txt"]).await.unwrap();

        let status = service.status().await.unwrap();
        assert!(status.staged.iter().any(|f| f.contains("staged.txt")));
    }

    #[tokio::test]
    async fn test_diff_no_changes() {
        let (_tmp, service) = create_test_repo().await;
        let diff = service.diff(false, None).await.unwrap();
        assert_eq!(diff, "No changes found.");
    }

    #[tokio::test]
    async fn test_diff_unstaged_changes() {
        let (tmp, service) = create_test_repo().await;

        // Create initial commit
        fs::write(tmp.path().join("file.txt"), "original").unwrap();
        service.run_git(&["add", "file.txt"]).await.unwrap();
        service.run_git(&["commit", "-m", "initial"]).await.unwrap();

        // Modify file without staging
        fs::write(tmp.path().join("file.txt"), "modified").unwrap();

        let diff = service.diff(false, None).await.unwrap();
        assert!(diff.contains("modified"));
    }

    #[tokio::test]
    async fn test_diff_staged_changes() {
        let (tmp, service) = create_test_repo().await;

        // Create initial commit
        fs::write(tmp.path().join("file.txt"), "original").unwrap();
        service.run_git(&["add", "file.txt"]).await.unwrap();
        service.run_git(&["commit", "-m", "initial"]).await.unwrap();

        // Stage a change
        fs::write(tmp.path().join("file.txt"), "staged change").unwrap();
        service.run_git(&["add", "file.txt"]).await.unwrap();

        let diff = service.diff(true, None).await.unwrap();
        assert!(diff.contains("staged change"));
    }

    #[tokio::test]
    async fn test_log_with_commits() {
        let (tmp, service) = create_test_repo().await;

        // Create two commits
        fs::write(tmp.path().join("a.txt"), "a").unwrap();
        service.run_git(&["add", "a.txt"]).await.unwrap();
        service.run_git(&["commit", "-m", "first"]).await.unwrap();

        fs::write(tmp.path().join("b.txt"), "b").unwrap();
        service.run_git(&["add", "b.txt"]).await.unwrap();
        service.run_git(&["commit", "-m", "second"]).await.unwrap();

        let log = service.log(10).await.unwrap();
        assert_eq!(log.len(), 2);
        assert_eq!(log[0].message, "second");
        assert_eq!(log[1].message, "first");
    }

    #[tokio::test]
    async fn test_log_max_count() {
        let (tmp, service) = create_test_repo().await;

        for i in 0..5 {
            fs::write(tmp.path().join(format!("{}.txt", i)), format!("{}", i)).unwrap();
            service
                .run_git(&["add", &format!("{}.txt", i)])
                .await
                .unwrap();
            service
                .run_git(&["commit", "-m", &format!("commit {}", i)])
                .await
                .unwrap();
        }

        let log = service.log(3).await.unwrap();
        assert_eq!(log.len(), 3);
    }

    #[tokio::test]
    async fn test_add_single_file() {
        let (tmp, service) = create_test_repo().await;

        fs::write(tmp.path().join("new_file.txt"), "content").unwrap();

        let result = service.add(&["new_file.txt".to_string()]).await;
        assert!(result.is_ok());
        let output = result.unwrap();
        assert_eq!(output.staged_files, vec!["new_file.txt"]);

        // Verify the file is actually staged
        let status = service.status().await.unwrap();
        assert!(status.staged.iter().any(|f| f.contains("new_file.txt")));
    }

    #[tokio::test]
    async fn test_add_multiple_files() {
        let (tmp, service) = create_test_repo().await;

        fs::write(tmp.path().join("a.txt"), "a").unwrap();
        fs::write(tmp.path().join("b.txt"), "b").unwrap();

        let result = service
            .add(&["a.txt".to_string(), "b.txt".to_string()])
            .await;
        assert!(result.is_ok());
        let output = result.unwrap();
        assert_eq!(output.staged_files.len(), 2);

        let status = service.status().await.unwrap();
        assert!(status.staged.iter().any(|f| f.contains("a.txt")));
        assert!(status.staged.iter().any(|f| f.contains("b.txt")));
    }

    #[tokio::test]
    async fn test_add_empty_paths() {
        let (_tmp, service) = create_test_repo().await;

        let result = service.add(&[]).await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("At least one path")
        );
    }

    #[tokio::test]
    async fn test_add_nonexistent_file() {
        let (_tmp, service) = create_test_repo().await;

        let result = service.add(&["does_not_exist.txt".to_string()]).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not exist"));
    }

    #[tokio::test]
    async fn test_add_outside_workspace() {
        let (_tmp, service) = create_test_repo().await;

        let result = service.add(&["../../etc/passwd".to_string()]).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_create_branch() {
        let (tmp, service) = create_test_repo().await;

        // Need at least one commit to create branches
        fs::write(tmp.path().join("init.txt"), "init").unwrap();
        service.run_git(&["add", "init.txt"]).await.unwrap();
        service.run_git(&["commit", "-m", "initial"]).await.unwrap();

        let result = service.create_branch("feature/test").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_switch_branch() {
        let (tmp, service) = create_test_repo().await;

        fs::write(tmp.path().join("init.txt"), "init").unwrap();
        service.run_git(&["add", "init.txt"]).await.unwrap();
        service.run_git(&["commit", "-m", "initial"]).await.unwrap();

        service.create_branch("test-branch").await.unwrap();
        let result = service.switch_branch("test-branch").await;
        assert!(result.is_ok());

        let status = service.status().await.unwrap();
        assert_eq!(status.branch, "test-branch");
    }

    #[tokio::test]
    async fn test_commit() {
        let (tmp, service) = create_test_repo().await;

        fs::write(tmp.path().join("commit_test.txt"), "content").unwrap();
        service.run_git(&["add", "commit_test.txt"]).await.unwrap();

        let result = service.commit("test commit message").await;
        assert!(result.is_ok());
        let output = result.unwrap();
        assert_eq!(output.message, "test commit message");
        assert!(!output.hash.is_empty());
    }

    #[tokio::test]
    async fn test_commit_no_staged_changes() {
        let (_tmp, service) = create_test_repo().await;

        let result = service.commit("empty commit").await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("No staged changes")
        );
    }

    #[tokio::test]
    async fn test_commit_empty_message() {
        let (_tmp, service) = create_test_repo().await;

        let result = service.commit("").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("cannot be empty"));
    }

    #[tokio::test]
    async fn test_validate_branch_name_valid() {
        assert!(GitService::validate_branch_name("feature/my-branch").is_ok());
        assert!(GitService::validate_branch_name("fix-123").is_ok());
        assert!(GitService::validate_branch_name("release/v1.0").is_ok());
    }

    #[tokio::test]
    async fn test_validate_branch_name_invalid() {
        // Original forbidden characters
        assert!(GitService::validate_branch_name("").is_err());
        assert!(GitService::validate_branch_name("branch name").is_err());
        assert!(GitService::validate_branch_name("branch..name").is_err());
        assert!(GitService::validate_branch_name("-branch").is_err());
        assert!(GitService::validate_branch_name("branch.lock").is_err());
        assert!(GitService::validate_branch_name("branch.").is_err());
        assert!(GitService::validate_branch_name("branch~1").is_err());
        assert!(GitService::validate_branch_name("branch^2").is_err());

        // Glob characters
        assert!(GitService::validate_branch_name("branch*").is_err());
        assert!(GitService::validate_branch_name("branch?name").is_err());
        assert!(GitService::validate_branch_name("branch[0]").is_err());

        // Consecutive slashes
        assert!(GitService::validate_branch_name("feat//branch").is_err());

        // Leading/trailing slash
        assert!(GitService::validate_branch_name("/branch").is_err());
        assert!(GitService::validate_branch_name("branch/").is_err());

        // Dot-prefixed path component
        assert!(GitService::validate_branch_name("feat/.hidden").is_err());
        assert!(GitService::validate_branch_name(".branch").is_err());
    }
}
