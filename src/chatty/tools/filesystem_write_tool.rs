use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::oneshot;
use tracing::{debug, warn};

use crate::chatty::models::execution_approval_store::notify_approval_via_global;
use crate::chatty::models::write_approval_store::{
    PendingWriteApprovals, WriteApprovalDecision, WriteApprovalRequest, WriteOperation,
};
use crate::chatty::services::filesystem_service::FileSystemService;
use crate::chatty::tools::filesystem_tool::FileSystemToolError;

/// Maximum wait time for user approval (5 minutes)
const APPROVAL_TIMEOUT: Duration = Duration::from_secs(300);

/// Maximum characters to show in content preview
const PREVIEW_MAX_CHARS: usize = 200;

/// Truncate a string for preview display
fn preview(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max])
    }
}

/// Request user approval for a write operation.
/// Posts a request to the shared pending approvals store, then waits for the UI to resolve it.
async fn request_write_approval(
    pending: &PendingWriteApprovals,
    operation: WriteOperation,
) -> Result<bool, anyhow::Error> {
    let id = uuid::Uuid::new_v4().to_string();
    let (tx, rx) = oneshot::channel();

    // Get description before moving operation into request
    let description = operation.description();

    let request = WriteApprovalRequest {
        id: id.clone(),
        operation,
        responder: tx,
    };

    // Insert the pending request
    {
        let mut store = pending.lock().unwrap();
        store.insert(id.clone(), request);
    }

    // Notify the stream so the UI can show an approval bar
    notify_approval_via_global(id.clone(), description, false);

    debug!(approval_id = %id, "Waiting for write approval");

    // Wait for user decision with timeout
    match tokio::time::timeout(APPROVAL_TIMEOUT, rx).await {
        Ok(Ok(WriteApprovalDecision::Approved)) => {
            debug!(approval_id = %id, "Write approved");
            Ok(true)
        }
        Ok(Ok(WriteApprovalDecision::Denied)) => {
            debug!(approval_id = %id, "Write denied");
            Ok(false)
        }
        Ok(Err(_)) => {
            warn!(approval_id = %id, "Approval channel closed");
            // Clean up
            let mut store = pending.lock().unwrap();
            store.remove(&id);
            Ok(false)
        }
        Err(_) => {
            warn!(approval_id = %id, "Write approval timed out");
            // Clean up
            let mut store = pending.lock().unwrap();
            store.remove(&id);
            Err(anyhow::anyhow!(
                "Write approval timed out after {} seconds",
                APPROVAL_TIMEOUT.as_secs()
            ))
        }
    }
}

// ─── write_file tool ───

#[derive(Deserialize, Serialize)]
pub struct WriteFileArgs {
    pub path: String,
    pub content: String,
}

#[derive(Debug, Serialize)]
pub struct WriteFileOutput {
    pub path: String,
    pub overwritten: bool,
    pub bytes_written: usize,
}

#[derive(Clone)]
pub struct WriteFileTool {
    service: Arc<FileSystemService>,
    pending_approvals: PendingWriteApprovals,
}

impl WriteFileTool {
    pub fn new(service: Arc<FileSystemService>, pending_approvals: PendingWriteApprovals) -> Self {
        Self {
            service,
            pending_approvals,
        }
    }
}

impl Tool for WriteFileTool {
    const NAME: &'static str = "write_file";
    type Error = FileSystemToolError;
    type Args = WriteFileArgs;
    type Output = WriteFileOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "write_file".to_string(),
            description: "Create or overwrite a file within the workspace. \
                         Requires user confirmation before writing. \
                         The file path must be within the workspace directory.\n\
                         \n\
                         Examples:\n\
                         - Create new file: {\"path\": \"src/utils.rs\", \"content\": \"pub fn hello() {}\"}\n\
                         - Write config: {\"path\": \"config.json\", \"content\": \"{\\\"key\\\": \\\"value\\\"}\"}"
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to write, relative to the workspace root"
                    },
                    "content": {
                        "type": "string",
                        "description": "Content to write to the file"
                    }
                },
                "required": ["path", "content"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        // Check if file exists to determine if this is an overwrite
        let is_overwrite = self.service.read_file(&args.path).await.is_ok();

        let operation = WriteOperation::WriteFile {
            path: args.path.clone(),
            is_overwrite,
            content_preview: preview(&args.content, PREVIEW_MAX_CHARS),
        };

        let approved = request_write_approval(&self.pending_approvals, operation).await?;
        if !approved {
            return Err(FileSystemToolError::OperationError(anyhow::anyhow!(
                "Write operation denied by user"
            )));
        }

        let bytes = args.content.len();
        let overwritten = self.service.write_file(&args.path, &args.content).await?;

        Ok(WriteFileOutput {
            path: args.path,
            overwritten,
            bytes_written: bytes,
        })
    }
}

// ─── create_directory tool ───

#[derive(Deserialize, Serialize)]
pub struct CreateDirectoryArgs {
    pub path: String,
}

#[derive(Debug, Serialize)]
pub struct CreateDirectoryOutput {
    pub path: String,
    pub already_existed: bool,
}

#[derive(Clone)]
pub struct CreateDirectoryTool {
    service: Arc<FileSystemService>,
}

impl CreateDirectoryTool {
    pub fn new(service: Arc<FileSystemService>) -> Self {
        Self { service }
    }
}

impl Tool for CreateDirectoryTool {
    const NAME: &'static str = "create_directory";
    type Error = FileSystemToolError;
    type Args = CreateDirectoryArgs;
    type Output = CreateDirectoryOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "create_directory".to_string(),
            description:
                "Create a directory (and any necessary parent directories) within the workspace. \
                         Does not require confirmation as it is non-destructive.\n\
                         \n\
                         Examples:\n\
                         - Create directory: {\"path\": \"src/components\"}\n\
                         - Create nested: {\"path\": \"tests/integration/fixtures\"}"
                    .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the directory to create, relative to the workspace root"
                    }
                },
                "required": ["path"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let already_existed = self.service.create_directory(&args.path).await?;
        Ok(CreateDirectoryOutput {
            path: args.path,
            already_existed,
        })
    }
}

// ─── delete_file tool ───

#[derive(Deserialize, Serialize)]
pub struct DeleteFileArgs {
    pub path: String,
}

#[derive(Debug, Serialize)]
pub struct DeleteFileOutput {
    pub path: String,
    pub deleted: bool,
}

#[derive(Clone)]
pub struct DeleteFileTool {
    service: Arc<FileSystemService>,
    pending_approvals: PendingWriteApprovals,
}

impl DeleteFileTool {
    pub fn new(service: Arc<FileSystemService>, pending_approvals: PendingWriteApprovals) -> Self {
        Self {
            service,
            pending_approvals,
        }
    }
}

impl Tool for DeleteFileTool {
    const NAME: &'static str = "delete_file";
    type Error = FileSystemToolError;
    type Args = DeleteFileArgs;
    type Output = DeleteFileOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "delete_file".to_string(),
            description: "Delete a file within the workspace. \
                         Requires user confirmation before deleting. \
                         This operation is irreversible.\n\
                         \n\
                         Examples:\n\
                         - Delete file: {\"path\": \"temp/output.log\"}\n\
                         - Remove old config: {\"path\": \"config.old.json\"}"
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to delete, relative to the workspace root"
                    }
                },
                "required": ["path"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let operation = WriteOperation::DeleteFile {
            path: args.path.clone(),
        };

        let approved = request_write_approval(&self.pending_approvals, operation).await?;
        if !approved {
            return Err(FileSystemToolError::OperationError(anyhow::anyhow!(
                "Delete operation denied by user"
            )));
        }

        self.service.delete_file(&args.path).await?;

        Ok(DeleteFileOutput {
            path: args.path,
            deleted: true,
        })
    }
}

// ─── move_file tool ───

#[derive(Deserialize, Serialize)]
pub struct MoveFileArgs {
    pub source: String,
    pub destination: String,
}

#[derive(Debug, Serialize)]
pub struct MoveFileOutput {
    pub source: String,
    pub destination: String,
}

#[derive(Clone)]
pub struct MoveFileTool {
    service: Arc<FileSystemService>,
    pending_approvals: PendingWriteApprovals,
}

impl MoveFileTool {
    pub fn new(service: Arc<FileSystemService>, pending_approvals: PendingWriteApprovals) -> Self {
        Self {
            service,
            pending_approvals,
        }
    }
}

impl Tool for MoveFileTool {
    const NAME: &'static str = "move_file";
    type Error = FileSystemToolError;
    type Args = MoveFileArgs;
    type Output = MoveFileOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "move_file".to_string(),
            description: "Move or rename a file within the workspace. \
                         Requires user confirmation. \
                         The destination must not already exist.\n\
                         \n\
                         Examples:\n\
                         - Rename: {\"source\": \"old_name.rs\", \"destination\": \"new_name.rs\"}\n\
                         - Move: {\"source\": \"file.txt\", \"destination\": \"archive/file.txt\"}"
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "source": {
                        "type": "string",
                        "description": "Current path of the file, relative to workspace root"
                    },
                    "destination": {
                        "type": "string",
                        "description": "New path for the file, relative to workspace root"
                    }
                },
                "required": ["source", "destination"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let operation = WriteOperation::MoveFile {
            source: args.source.clone(),
            destination: args.destination.clone(),
        };

        let approved = request_write_approval(&self.pending_approvals, operation).await?;
        if !approved {
            return Err(FileSystemToolError::OperationError(anyhow::anyhow!(
                "Move operation denied by user"
            )));
        }

        self.service
            .move_file(&args.source, &args.destination)
            .await?;

        Ok(MoveFileOutput {
            source: args.source,
            destination: args.destination,
        })
    }
}

// ─── apply_diff tool ───

#[derive(Deserialize, Serialize)]
pub struct ApplyDiffArgs {
    pub path: String,
    pub old_content: String,
    pub new_content: String,
}

#[derive(Debug, Serialize)]
pub struct ApplyDiffOutput {
    pub path: String,
    pub insertions: usize,
    pub deletions: usize,
}

#[derive(Clone)]
pub struct ApplyDiffTool {
    service: Arc<FileSystemService>,
    pending_approvals: PendingWriteApprovals,
}

impl ApplyDiffTool {
    pub fn new(service: Arc<FileSystemService>, pending_approvals: PendingWriteApprovals) -> Self {
        Self {
            service,
            pending_approvals,
        }
    }
}

impl Tool for ApplyDiffTool {
    const NAME: &'static str = "apply_diff";
    type Error = FileSystemToolError;
    type Args = ApplyDiffArgs;
    type Output = ApplyDiffOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "apply_diff".to_string(),
            description: "Apply a targeted edit to a file by replacing specific content. \
                         Requires user confirmation. \
                         Provide the exact old content to find and the new content to replace it with. \
                         Only the first occurrence is replaced.\n\
                         \n\
                         Examples:\n\
                         - Fix typo: {\"path\": \"README.md\", \"old_content\": \"teh\", \"new_content\": \"the\"}\n\
                         - Update function: {\"path\": \"src/main.rs\", \"old_content\": \"fn old()\", \"new_content\": \"fn new()\"}"
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to edit, relative to workspace root"
                    },
                    "old_content": {
                        "type": "string",
                        "description": "Exact content to find in the file (will be replaced)"
                    },
                    "new_content": {
                        "type": "string",
                        "description": "New content to replace the old content with"
                    }
                },
                "required": ["path", "old_content", "new_content"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let operation = WriteOperation::ApplyDiff {
            path: args.path.clone(),
            old_preview: preview(&args.old_content, PREVIEW_MAX_CHARS),
            new_preview: preview(&args.new_content, PREVIEW_MAX_CHARS),
        };

        let approved = request_write_approval(&self.pending_approvals, operation).await?;
        if !approved {
            return Err(FileSystemToolError::OperationError(anyhow::anyhow!(
                "Diff operation denied by user"
            )));
        }

        let result = self
            .service
            .apply_diff(&args.path, &args.old_content, &args.new_content)
            .await?;

        Ok(ApplyDiffOutput {
            path: result.path,
            insertions: result.insertions,
            deletions: result.deletions,
        })
    }
}
