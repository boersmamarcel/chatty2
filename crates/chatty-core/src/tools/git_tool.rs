use rig_agent::tool::{Tool, ToolContext, ToolExecutionError};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::models::execution_approval_store::{PendingApprovals, request_execution_approval};
use crate::services::git_service::{
    GitAddOutput, GitCommitOutput, GitLogEntry, GitMergeOutput, GitService, GitStatusOutput,
};
use crate::settings::models::execution_settings::ApprovalMode;
use crate::tools::ToolError;

// ── GitStatusTool ───────────────────────────────────────────────────────────

#[derive(Deserialize, Serialize)]
pub struct GitStatusArgs {}

// Service output types are reused directly as tool output (same as GitLogEntry).
// If tool-specific formatting diverges in the future, introduce a wrapper then.

/// Check the current status of the git repository.
#[derive(Clone)]
pub struct GitStatusTool {
    service: Arc<GitService>,
}

impl GitStatusTool {
    pub fn new(service: Arc<GitService>) -> Self {
        Self { service }
    }
}

impl Tool for GitStatusTool {
    const NAME: &'static str = "git_status";
    type Error = ToolError;
    type Args = GitStatusArgs;
    type Output = GitStatusOutput;

    fn description(&self) -> String {
        "Check the current status of the git repository. Shows the current \
                         branch, staged changes, unstaged modifications, and untracked files. \
                         Use this to understand the state of the working tree before making changes."
                .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {},
            "required": []
        })
    }

    /// Keep the real failure text in front of the user and the model:
    /// rig's default `map_error` redacts it to "the tool failed" (AGE-187).
    fn map_error(&self, error: Self::Error) -> ToolExecutionError {
        crate::tools::map_tool_error(Self::NAME, error)
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        _args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        tracing::debug!("Checking git status");
        let status = self.service.status().await?;
        Ok(status)
    }
}

// ── GitDiffTool ─────────────────────────────────────────────────────────────

#[derive(Deserialize, Serialize)]
pub struct GitDiffArgs {
    /// If true, show staged changes (--cached). Defaults to false.
    #[serde(default)]
    pub staged: bool,
    /// Optional file path to restrict the diff to.
    #[serde(default)]
    pub path: Option<String>,
    /// Optional `base..head` range: what `head` changed relative to `base`,
    /// independent of the working tree (AGE-404). Takes precedence over
    /// `staged`.
    #[serde(default)]
    pub range: Option<String>,
}

/// Split a `base..head` range into its two refs. Two dots exactly: a
/// three-dot range diffs against the merge base, which is not what a
/// reviewer reading a branch asked for.
fn parse_range(range: &str) -> Result<(&str, &str), ToolError> {
    match range.trim().split_once("..") {
        Some((base, head)) if !base.is_empty() && !head.is_empty() && !head.starts_with('.') => {
            Ok((base, head))
        }
        _ => Err(ToolError::OperationFailed(format!(
            "range must be 'base..head' (e.g. 'main..sub-agent/w1'), got '{range}'"
        ))),
    }
}

#[derive(Debug, Serialize)]
pub struct GitDiffOutput {
    pub diff: String,
}

/// View changes in the git repository.
#[derive(Clone)]
pub struct GitDiffTool {
    service: Arc<GitService>,
}

impl GitDiffTool {
    pub fn new(service: Arc<GitService>) -> Self {
        Self { service }
    }
}

impl Tool for GitDiffTool {
    const NAME: &'static str = "git_diff";
    type Error = ToolError;
    type Args = GitDiffArgs;
    type Output = GitDiffOutput;

    fn description(&self) -> String {
        "View changes in the git repository. By default shows unstaged changes. \
                         Set 'staged' to true to see changes that have been staged for commit. \
                         Set 'range' to 'base..head' (e.g. 'main..sub-agent/w1') to see what a \
                         branch changed relative to another, independent of the working tree. \
                         Optionally specify a 'path' to limit the diff to a specific file."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "staged": {
                    "type": "boolean",
                    "description": "If true, show staged changes (git diff --cached). Default: false"
                },
                "path": {
                    "type": "string",
                    "description": "Optional file path to restrict the diff to"
                },
                "range": {
                    "type": "string",
                    "description": "Optional 'base..head' range (e.g. 'main..sub-agent/w1'): diff the two refs instead of the working tree"
                }
            },
            "required": ["staged", "path", "range"]
        })
    }

    /// Keep the real failure text in front of the user and the model:
    /// rig's default `map_error` redacts it to "the tool failed" (AGE-187).
    fn map_error(&self, error: Self::Error) -> ToolExecutionError {
        crate::tools::map_tool_error(Self::NAME, error)
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        tracing::debug!(staged = args.staged, path = ?args.path, range = ?args.range, "Getting git diff");
        let diff = match args
            .range
            .as_deref()
            .map(str::trim)
            .filter(|r| !r.is_empty())
        {
            Some(range) => {
                let (base, head) = parse_range(range)?;
                self.service
                    .diff_range(base, head, args.path.as_deref())
                    .await?
            }
            None => self.service.diff(args.staged, args.path.as_deref()).await?,
        };
        Ok(GitDiffOutput { diff })
    }
}

// ── GitLogTool ──────────────────────────────────────────────────────────────

#[derive(Deserialize, Serialize)]
pub struct GitLogArgs {
    /// Maximum number of commits to show. Defaults to 10.
    #[serde(default = "default_max_count")]
    pub max_count: u32,
}

fn default_max_count() -> u32 {
    10
}

#[derive(Debug, Serialize)]
pub struct GitLogOutput {
    pub commits: Vec<GitLogEntry>,
    pub count: usize,
}

/// View the commit history of the repository.
#[derive(Clone)]
pub struct GitLogTool {
    service: Arc<GitService>,
}

impl GitLogTool {
    pub fn new(service: Arc<GitService>) -> Self {
        Self { service }
    }
}

impl Tool for GitLogTool {
    const NAME: &'static str = "git_log";
    type Error = ToolError;
    type Args = GitLogArgs;
    type Output = GitLogOutput;

    fn description(&self) -> String {
        "View recent commit history. Returns commit hash, author, date, and \
                         message for each commit. Use 'max_count' to control how many commits \
                         to show (default: 10, max: 100)."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "max_count": {
                    "type": "integer",
                    "description": "Maximum number of commits to return (default: 10, max: 100)"
                }
            },
            "required": ["max_count"]
        })
    }

    /// Keep the real failure text in front of the user and the model:
    /// rig's default `map_error` redacts it to "the tool failed" (AGE-187).
    fn map_error(&self, error: Self::Error) -> ToolExecutionError {
        crate::tools::map_tool_error(Self::NAME, error)
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let max_count = args.max_count.min(100); // Cap at 100
        tracing::debug!(max_count, "Getting git log");
        let commits = self.service.log(max_count).await?;
        let count = commits.len();
        Ok(GitLogOutput { commits, count })
    }
}

// ── GitAddTool ──────────────────────────────────────────────────────────────

#[derive(Deserialize, Serialize)]
pub struct GitAddArgs {
    /// List of file paths to stage (relative to workspace root).
    pub paths: Vec<String>,
}

/// Stage files for the next commit.
#[derive(Clone)]
pub struct GitAddTool {
    service: Arc<GitService>,
    approval_mode: ApprovalMode,
    pending_approvals: PendingApprovals,
}

impl GitAddTool {
    pub fn new(
        service: Arc<GitService>,
        approval_mode: ApprovalMode,
        pending_approvals: PendingApprovals,
    ) -> Self {
        Self {
            service,
            approval_mode,
            pending_approvals,
        }
    }
}

impl Tool for GitAddTool {
    const NAME: &'static str = "git_add";
    type Error = ToolError;
    type Args = GitAddArgs;
    type Output = GitAddOutput;

    fn description(&self) -> String {
        "Stage files for the next git commit. Provide a list of file paths \
                         (relative to the workspace root) to add to the staging area. Each path \
                         must point to an existing file or directory within the workspace. Use \
                         git_status first to see which files have changes that can be staged."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "paths": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "List of file paths to stage (relative to workspace root)"
                }
            },
            "required": ["paths"]
        })
    }

    /// Keep the real failure text in front of the user and the model:
    /// rig's default `map_error` redacts it to "the tool failed" (AGE-187).
    fn map_error(&self, error: Self::Error) -> ToolExecutionError {
        crate::tools::map_tool_error(Self::NAME, error)
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let file_list = args.paths.join(", ");
        let approved = request_execution_approval(
            &self.pending_approvals,
            &self.approval_mode,
            &format!("[git] stage files: {}", file_list),
            false,
        )
        .await?;

        if !approved {
            return Err(ToolError::OperationFailed(
                "Staging denied by user".to_string(),
            ));
        }

        tracing::debug!(paths = ?args.paths, "Staging files");
        let result = self.service.add(&args.paths).await?;
        Ok(result)
    }
}

// ── GitCreateBranchTool ─────────────────────────────────────────────────────

#[derive(Deserialize, Serialize)]
pub struct GitCreateBranchArgs {
    /// Name of the branch to create.
    pub name: String,
}

#[derive(Debug, Serialize)]
pub struct GitCreateBranchOutput {
    pub success: bool,
    pub message: String,
}

/// Create a new git branch.
#[derive(Clone)]
pub struct GitCreateBranchTool {
    service: Arc<GitService>,
    approval_mode: ApprovalMode,
    pending_approvals: PendingApprovals,
}

impl GitCreateBranchTool {
    pub fn new(
        service: Arc<GitService>,
        approval_mode: ApprovalMode,
        pending_approvals: PendingApprovals,
    ) -> Self {
        Self {
            service,
            approval_mode,
            pending_approvals,
        }
    }
}

impl Tool for GitCreateBranchTool {
    const NAME: &'static str = "git_create_branch";
    type Error = ToolError;
    type Args = GitCreateBranchArgs;
    type Output = GitCreateBranchOutput;

    fn description(&self) -> String {
        "Create a new git branch from the current HEAD. The branch name must \
                         follow git naming rules (no spaces, no '..', cannot start with '-', etc.). \
                         This does NOT switch to the new branch — use git_switch_branch for that."
                .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "The name of the new branch to create"
                }
            },
            "required": ["name"]
        })
    }

    /// Keep the real failure text in front of the user and the model:
    /// rig's default `map_error` redacts it to "the tool failed" (AGE-187).
    fn map_error(&self, error: Self::Error) -> ToolExecutionError {
        crate::tools::map_tool_error(Self::NAME, error)
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let approved = request_execution_approval(
            &self.pending_approvals,
            &self.approval_mode,
            &format!("[git] create branch '{}'", args.name),
            false,
        )
        .await?;

        if !approved {
            return Err(ToolError::OperationFailed(
                "Branch creation denied by user".to_string(),
            ));
        }

        tracing::debug!(name = %args.name, "Creating git branch");
        let message = self.service.create_branch(&args.name).await?;
        Ok(GitCreateBranchOutput {
            success: true,
            message,
        })
    }
}

// ── GitSwitchBranchTool ─────────────────────────────────────────────────────

#[derive(Deserialize, Serialize)]
pub struct GitSwitchBranchArgs {
    /// Name of the branch to switch to.
    pub name: String,
}

#[derive(Debug, Serialize)]
pub struct GitSwitchBranchOutput {
    pub success: bool,
    pub message: String,
}

/// Switch to an existing git branch.
#[derive(Clone)]
pub struct GitSwitchBranchTool {
    service: Arc<GitService>,
    approval_mode: ApprovalMode,
    pending_approvals: PendingApprovals,
}

impl GitSwitchBranchTool {
    pub fn new(
        service: Arc<GitService>,
        approval_mode: ApprovalMode,
        pending_approvals: PendingApprovals,
    ) -> Self {
        Self {
            service,
            approval_mode,
            pending_approvals,
        }
    }
}

impl Tool for GitSwitchBranchTool {
    const NAME: &'static str = "git_switch_branch";
    type Error = ToolError;
    type Args = GitSwitchBranchArgs;
    type Output = GitSwitchBranchOutput;

    fn description(&self) -> String {
        "Switch to an existing git branch. The branch must already exist — \
                         use git_create_branch to create a new one first. This will fail if \
                         there are uncommitted changes that conflict with the target branch."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "The name of the branch to switch to"
                }
            },
            "required": ["name"]
        })
    }

    /// Keep the real failure text in front of the user and the model:
    /// rig's default `map_error` redacts it to "the tool failed" (AGE-187).
    fn map_error(&self, error: Self::Error) -> ToolExecutionError {
        crate::tools::map_tool_error(Self::NAME, error)
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let approved = request_execution_approval(
            &self.pending_approvals,
            &self.approval_mode,
            &format!("[git] switch to branch '{}'", args.name),
            false,
        )
        .await?;

        if !approved {
            return Err(ToolError::OperationFailed(
                "Branch switch denied by user".to_string(),
            ));
        }

        tracing::debug!(name = %args.name, "Switching git branch");
        let message = self.service.switch_branch(&args.name).await?;
        Ok(GitSwitchBranchOutput {
            success: true,
            message,
        })
    }
}

// ── GitCommitTool ───────────────────────────────────────────────────────────

#[derive(Deserialize, Serialize)]
pub struct GitCommitArgs {
    /// The commit message.
    pub message: String,
}

/// Commit staged changes with a message.
#[derive(Clone)]
pub struct GitCommitTool {
    service: Arc<GitService>,
    approval_mode: ApprovalMode,
    pending_approvals: PendingApprovals,
}

impl GitCommitTool {
    pub fn new(
        service: Arc<GitService>,
        approval_mode: ApprovalMode,
        pending_approvals: PendingApprovals,
    ) -> Self {
        Self {
            service,
            approval_mode,
            pending_approvals,
        }
    }
}

impl Tool for GitCommitTool {
    const NAME: &'static str = "git_commit";
    type Error = ToolError;
    type Args = GitCommitArgs;
    type Output = GitCommitOutput;

    fn description(&self) -> String {
        "Commit staged changes with a message. Only commits changes that \
                         have been previously staged with 'git add'. Returns an error if there \
                         are no staged changes. Use git_status first to check what's staged. \
                         The commit message should be clear and descriptive."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "message": {
                    "type": "string",
                    "description": "The commit message describing the changes"
                }
            },
            "required": ["message"]
        })
    }

    /// Keep the real failure text in front of the user and the model:
    /// rig's default `map_error` redacts it to "the tool failed" (AGE-187).
    fn map_error(&self, error: Self::Error) -> ToolExecutionError {
        crate::tools::map_tool_error(Self::NAME, error)
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let approved = request_execution_approval(
            &self.pending_approvals,
            &self.approval_mode,
            &format!("[git] commit with message: \"{}\"", args.message),
            false,
        )
        .await?;

        if !approved {
            return Err(ToolError::OperationFailed(
                "Commit denied by user".to_string(),
            ));
        }

        tracing::debug!(message = %args.message, "Creating git commit");
        let result = self.service.commit(&args.message).await?;
        Ok(result)
    }
}

// ── GitMergeTool ────────────────────────────────────────────────────────────

#[derive(Deserialize, Serialize)]
pub struct GitMergeArgs {
    /// Name of the branch to merge into the current branch.
    pub branch: String,
    /// If true, always create a merge commit (--no-ff). Defaults to false.
    #[serde(default)]
    pub no_ff: bool,
    /// Optional merge commit message.
    #[serde(default)]
    pub message: Option<String>,
}

/// Merge a branch into the current branch (AGE-404): how a coordinator
/// without a shell takes a worker's `sub-agent/<name>` branch.
#[derive(Clone)]
pub struct GitMergeTool {
    service: Arc<GitService>,
    approval_mode: ApprovalMode,
    pending_approvals: PendingApprovals,
}

impl GitMergeTool {
    pub fn new(
        service: Arc<GitService>,
        approval_mode: ApprovalMode,
        pending_approvals: PendingApprovals,
    ) -> Self {
        Self {
            service,
            approval_mode,
            pending_approvals,
        }
    }
}

impl Tool for GitMergeTool {
    const NAME: &'static str = "git_merge";
    type Error = ToolError;
    type Args = GitMergeArgs;
    type Output = GitMergeOutput;

    fn description(&self) -> String {
        "Merge an existing branch into the current branch (git merge). Set \
                         'no_ff' to true to always record a merge commit, and 'message' to \
                         name it. On a conflict the error lists the conflicting files and the \
                         tree is left in the conflicted state: report them, do not resolve \
                         them by hand."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "branch": {
                    "type": "string",
                    "description": "The name of the branch to merge into the current branch"
                },
                "no_ff": {
                    "type": "boolean",
                    "description": "If true, always create a merge commit (git merge --no-ff). Default: false"
                },
                "message": {
                    "type": "string",
                    "description": "Optional merge commit message"
                }
            },
            "required": ["branch", "no_ff", "message"]
        })
    }

    /// Keep the real failure text in front of the user and the model:
    /// rig's default `map_error` redacts it to "the tool failed" (AGE-187).
    fn map_error(&self, error: Self::Error) -> ToolExecutionError {
        crate::tools::map_tool_error(Self::NAME, error)
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let approved = request_execution_approval(
            &self.pending_approvals,
            &self.approval_mode,
            &format!(
                "[git] merge branch '{}'{}",
                args.branch,
                if args.no_ff { " (--no-ff)" } else { "" }
            ),
            false,
        )
        .await?;

        if !approved {
            return Err(ToolError::OperationFailed(
                "Merge denied by user".to_string(),
            ));
        }

        tracing::debug!(branch = %args.branch, no_ff = args.no_ff, "Merging git branch");
        let result = self
            .service
            .merge(&args.branch, args.no_ff, args.message.as_deref())
            .await?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AGE-404: a reviewer copies `<default>..<branch>` out of the skill
    /// text; anything else is rejected before it reaches git.
    #[test]
    fn parse_range_takes_base_dot_dot_head_only() {
        assert_eq!(
            parse_range("main..sub-agent/w1").unwrap(),
            ("main", "sub-agent/w1")
        );
        assert_eq!(parse_range(" master..feat ").unwrap(), ("master", "feat"));
        for bad in ["main", "main..", "..feat", "main...feat", ""] {
            assert!(parse_range(bad).is_err(), "should reject {bad:?}");
        }
    }
}
