use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::chatty::models::execution_approval_store::{
    ApprovalDecision, ExecutionApprovalRequest, PendingApprovals,
};
use crate::chatty::services::git_service::{
    GitCommitOutput, GitLogEntry, GitService, GitStatusOutput,
};
use crate::settings::models::execution_settings::ApprovalMode;

// ── Error type ──────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum GitToolError {
    #[error("Git error: {0}")]
    GitError(#[from] anyhow::Error),
}

// ── Approval helper ─────────────────────────────────────────────────────────

/// Request user approval for a git write operation.
///
/// Follows the same pattern as shell_tool.rs approval flow.
async fn request_git_approval(
    pending_approvals: &PendingApprovals,
    approval_mode: &ApprovalMode,
    description: &str,
) -> anyhow::Result<bool> {
    match approval_mode {
        ApprovalMode::AutoApproveAll => Ok(true),
        // Git operations are never sandboxed, so AutoApproveSandboxed behaves like AlwaysAsk
        ApprovalMode::AutoApproveSandboxed | ApprovalMode::AlwaysAsk => {
            let (tx, rx) = tokio::sync::oneshot::channel();
            let request_id = uuid::Uuid::new_v4().to_string();

            let request = ExecutionApprovalRequest {
                id: request_id.clone(),
                command: format!("[git] {}", description),
                is_sandboxed: false,
                created_at: std::time::SystemTime::now(),
                responder: tx,
            };

            {
                let mut pending = pending_approvals.lock().unwrap();
                pending.insert(request_id.clone(), request);
            }

            crate::chatty::models::execution_approval_store::notify_approval_via_global(
                request_id.clone(),
                format!("[git] {}", description),
                false,
            );

            match tokio::time::timeout(std::time::Duration::from_secs(300), rx).await {
                Ok(Ok(ApprovalDecision::Approved)) => Ok(true),
                Ok(Ok(ApprovalDecision::Denied)) => Ok(false),
                Ok(Err(_)) => Err(anyhow::anyhow!("Approval channel closed")),
                Err(_) => {
                    let mut pending = pending_approvals.lock().unwrap();
                    pending.remove(&request_id);
                    Err(anyhow::anyhow!("Approval timeout (5 minutes)"))
                }
            }
        }
    }
}

// ── GitStatusTool ───────────────────────────────────────────────────────────

#[derive(Deserialize, Serialize)]
pub struct GitStatusArgs {}

#[derive(Debug, Serialize)]
pub struct GitStatusToolOutput {
    pub branch: String,
    pub staged: Vec<String>,
    pub modified: Vec<String>,
    pub untracked: Vec<String>,
    pub raw: String,
}

impl From<GitStatusOutput> for GitStatusToolOutput {
    fn from(s: GitStatusOutput) -> Self {
        Self {
            branch: s.branch,
            staged: s.staged,
            modified: s.modified,
            untracked: s.untracked,
            raw: s.raw,
        }
    }
}

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
    type Error = GitToolError;
    type Args = GitStatusArgs;
    type Output = GitStatusToolOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "git_status".to_string(),
            description: "Check the current status of the git repository. Shows the current \
                         branch, staged changes, unstaged modifications, and untracked files. \
                         Use this to understand the state of the working tree before making changes."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        }
    }

    async fn call(&self, _args: Self::Args) -> Result<Self::Output, Self::Error> {
        tracing::debug!("Checking git status");
        let status = self.service.status().await?;
        Ok(status.into())
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
    type Error = GitToolError;
    type Args = GitDiffArgs;
    type Output = GitDiffOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "git_diff".to_string(),
            description: "View changes in the git repository. By default shows unstaged changes. \
                         Set 'staged' to true to see changes that have been staged for commit. \
                         Optionally specify a 'path' to limit the diff to a specific file."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "staged": {
                        "type": "boolean",
                        "description": "If true, show staged changes (git diff --cached). Default: false"
                    },
                    "path": {
                        "type": "string",
                        "description": "Optional file path to restrict the diff to"
                    }
                },
                "required": []
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        tracing::debug!(staged = args.staged, path = ?args.path, "Getting git diff");
        let diff = self.service.diff(args.staged, args.path.as_deref()).await?;
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
    type Error = GitToolError;
    type Args = GitLogArgs;
    type Output = GitLogOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "git_log".to_string(),
            description: "View recent commit history. Returns commit hash, author, date, and \
                         message for each commit. Use 'max_count' to control how many commits \
                         to show (default: 10, max: 100)."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "max_count": {
                        "type": "integer",
                        "description": "Maximum number of commits to return (default: 10, max: 100)"
                    }
                },
                "required": []
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let max_count = args.max_count.min(100); // Cap at 100
        tracing::debug!(max_count, "Getting git log");
        let commits = self.service.log(max_count).await?;
        let count = commits.len();
        Ok(GitLogOutput { commits, count })
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
    type Error = GitToolError;
    type Args = GitCreateBranchArgs;
    type Output = GitCreateBranchOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "git_create_branch".to_string(),
            description: "Create a new git branch from the current HEAD. The branch name must \
                         follow git naming rules (no spaces, no '..', cannot start with '-', etc.). \
                         This does NOT switch to the new branch — use git_switch_branch for that."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "The name of the new branch to create"
                    }
                },
                "required": ["name"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let approved = request_git_approval(
            &self.pending_approvals,
            &self.approval_mode,
            &format!("create branch '{}'", args.name),
        )
        .await?;

        if !approved {
            return Err(GitToolError::GitError(anyhow::anyhow!(
                "Branch creation denied by user"
            )));
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
    type Error = GitToolError;
    type Args = GitSwitchBranchArgs;
    type Output = GitSwitchBranchOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "git_switch_branch".to_string(),
            description: "Switch to an existing git branch. The branch must already exist — \
                         use git_create_branch to create a new one first. This will fail if \
                         there are uncommitted changes that conflict with the target branch."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "The name of the branch to switch to"
                    }
                },
                "required": ["name"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let approved = request_git_approval(
            &self.pending_approvals,
            &self.approval_mode,
            &format!("switch to branch '{}'", args.name),
        )
        .await?;

        if !approved {
            return Err(GitToolError::GitError(anyhow::anyhow!(
                "Branch switch denied by user"
            )));
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

#[derive(Debug, Serialize)]
pub struct GitCommitToolOutput {
    pub hash: String,
    pub message: String,
    pub summary: String,
}

impl From<GitCommitOutput> for GitCommitToolOutput {
    fn from(c: GitCommitOutput) -> Self {
        Self {
            hash: c.hash,
            message: c.message,
            summary: c.summary,
        }
    }
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
    type Error = GitToolError;
    type Args = GitCommitArgs;
    type Output = GitCommitToolOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "git_commit".to_string(),
            description: "Commit staged changes with a message. Only commits changes that \
                         have been previously staged with 'git add'. Returns an error if there \
                         are no staged changes. Use git_status first to check what's staged. \
                         The commit message should be clear and descriptive."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "message": {
                        "type": "string",
                        "description": "The commit message describing the changes"
                    }
                },
                "required": ["message"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let approved = request_git_approval(
            &self.pending_approvals,
            &self.approval_mode,
            &format!("commit with message: \"{}\"", args.message),
        )
        .await?;

        if !approved {
            return Err(GitToolError::GitError(anyhow::anyhow!(
                "Commit denied by user"
            )));
        }

        tracing::debug!(message = %args.message, "Creating git commit");
        let result = self.service.commit(&args.message).await?;
        Ok(result.into())
    }
}
