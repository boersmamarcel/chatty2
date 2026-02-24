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
        // Disable commit signing so tests work in environments where no signing
        // key / server is available (e.g. CI without GPG/SSH agent).
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
