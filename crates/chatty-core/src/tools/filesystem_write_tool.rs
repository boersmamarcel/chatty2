use rig_agent::tool::{Tool, ToolContext, ToolExecutionError};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::oneshot;
use tracing::{debug, warn};

use crate::models::execution_approval_store::ApprovalNotification;
use crate::models::write_approval_store::{
    PendingWriteApprovals, WriteApprovalDecision, WriteApprovalRequest, WriteOperation,
};
use crate::services::filesystem_service::FileSystemService;
use crate::settings::models::execution_settings::ApprovalMode;
use crate::tools::ToolError;

/// Maximum wait time for user approval (5 minutes)
const APPROVAL_TIMEOUT: Duration = Duration::from_secs(300);

/// Maximum characters to show in content preview
const PREVIEW_MAX_CHARS: usize = 200;

/// Truncate a string for preview display (character-safe for UTF-8).
fn preview(s: &str, max_chars: usize) -> String {
    match s.char_indices().nth(max_chars) {
        None => s.to_string(),
        Some((byte_ix, _)) => format!("{}...", &s[..byte_ix]),
    }
}

/// Request user approval for a write operation.
/// Posts a request to the shared pending approvals store, then waits for the UI to resolve it.
/// If `approval_mode` is `AutoApproveAll` or `AutoApproveSandboxed`, approves immediately
/// without user interaction. `AlwaysAsk` prompts for each write.
///
/// The mode is the agent's own (`ExecutionSettingsModel::approval_mode` from
/// its `AgentBuildContext`), never a process-wide setting, so two agents in
/// one process can run under different policies (AGE-193).
pub async fn request_write_approval(
    pending: &PendingWriteApprovals,
    approval_mode: &ApprovalMode,
    operation: WriteOperation,
) -> Result<bool, anyhow::Error> {
    if matches!(
        approval_mode,
        ApprovalMode::AutoApproveAll | ApprovalMode::AutoApproveSandboxed
    ) {
        return Ok(true);
    }

    let id = uuid::Uuid::new_v4().to_string();
    let (tx, rx) = oneshot::channel();

    // Get description before moving operation into request
    let description = operation.description();

    let request = WriteApprovalRequest {
        id: id.clone(),
        operation,
        responder: tx,
    };

    // Insert the pending request and notify through the store's own
    // per-turn notifier so the UI can show an approval bar (AGE-246 / D7).
    {
        let mut state = pending.lock();
        state.requests.insert(id.clone(), request);
        match &state.notifier {
            Some(tx) => {
                if let Err(e) = tx.send(ApprovalNotification {
                    id: id.clone(),
                    command: description,
                    is_sandboxed: false,
                }) {
                    warn!(approval_id = %id, error = ?e, "Failed to send write approval notification");
                }
            }
            None => {
                warn!(approval_id = %id, "Write approval notifier not set - notification not sent!");
            }
        }
    }

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
            pending.lock().requests.remove(&id);
            Ok(false)
        }
        Err(_) => {
            warn!(approval_id = %id, "Write approval timed out");
            // Clean up
            pending.lock().requests.remove(&id);
            Err(anyhow::anyhow!(
                "Write approval timed out after {} seconds",
                APPROVAL_TIMEOUT.as_secs()
            ))
        }
    }
}

// ─── final_answer tool ───

#[derive(Deserialize, Serialize)]
pub struct FinalAnswerArgs {
    pub answer: String,
    #[serde(default)]
    pub output_path: Option<String>,
    #[serde(default)]
    pub guidance: Option<String>,
    #[serde(default)]
    pub format_hint: Option<String>,
    #[serde(default)]
    pub trailing_newline: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct FinalAnswerOutput {
    pub path: String,
    pub answer: String,
    pub overwritten: bool,
    pub bytes_written: usize,
    pub notes: Vec<String>,
}

#[derive(Clone)]
pub struct FinalAnswerTool {
    service: Arc<FileSystemService>,
    approval_mode: ApprovalMode,
    pending_approvals: PendingWriteApprovals,
}

impl FinalAnswerTool {
    pub fn new(
        service: Arc<FileSystemService>,
        approval_mode: ApprovalMode,
        pending_approvals: PendingWriteApprovals,
    ) -> Self {
        Self {
            service,
            approval_mode,
            pending_approvals,
        }
    }
}

impl Tool for FinalAnswerTool {
    const NAME: &'static str = "final_answer";
    type Error = ToolError;
    type Args = FinalAnswerArgs;
    type Output = FinalAnswerOutput;

    fn description(&self) -> String {
        "Normalize and write a final answer to a workspace file. \
                          Use this instead of write_file for benchmark/factoid tasks \
                          once you have the answer candidate. Defaults to answer.txt \
                          in the workspace and keeps output compact."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "answer": {
                    "type": "string",
                    "description": "The final answer candidate, not the full reasoning."
                },
                "output_path": {
                    "type": "string",
                    "description": "Optional output path. Defaults to answer.txt. Use /app/answer.txt when the task explicitly requires it."
                },
                "guidance": {
                    "type": "string",
                    "description": "Optional task formatting guidance, such as rounding, yes/no, or comma-separated output."
                },
                "format_hint": {
                    "type": "string",
                    "description": "Optional explicit format hint: scalar, numeric, yes_no, comma_separated, or multiline."
                },
                "trailing_newline": {
                    "type": "boolean",
                    "description": "Whether to end the written file with a newline. Defaults to true."
                }
            },
            "required": ["answer"]
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
        let path = args.output_path.unwrap_or_else(|| "answer.txt".to_string());
        let (answer, notes) = normalize_final_answer(
            &args.answer,
            args.guidance.as_deref(),
            args.format_hint.as_deref(),
        );
        if answer.is_empty() {
            return Err(ToolError::OperationFailed(
                "final_answer received an empty answer after normalization".to_string(),
            ));
        }

        let content = if args.trailing_newline.unwrap_or(true) {
            format!("{answer}\n")
        } else {
            answer.clone()
        };
        let is_overwrite = self.service.read_file(&path).await.is_ok();
        let operation = WriteOperation::WriteFile {
            path: path.clone(),
            is_overwrite,
            content_preview: preview(&content, PREVIEW_MAX_CHARS),
        };

        let approved =
            request_write_approval(&self.pending_approvals, &self.approval_mode, operation).await?;
        if !approved {
            return Err(ToolError::OperationFailed(
                "Final answer write denied by user".to_string(),
            ));
        }

        let bytes = content.len();
        let overwritten = self.service.write_file(&path, &content).await?;

        Ok(FinalAnswerOutput {
            path,
            answer,
            overwritten,
            bytes_written: bytes,
            notes,
        })
    }
}

fn normalize_final_answer(
    raw: &str,
    guidance: Option<&str>,
    format_hint: Option<&str>,
) -> (String, Vec<String>) {
    let mut notes = Vec::new();
    let mut answer = strip_code_fence(raw.trim()).trim().to_string();

    if answer.len() >= 2
        && ((answer.starts_with('"') && answer.ends_with('"'))
            || (answer.starts_with('\'') && answer.ends_with('\'')))
    {
        answer = answer[1..answer.len() - 1].trim().to_string();
        notes.push("removed surrounding quotes".to_string());
    }

    let hint = format_hint.unwrap_or_default().to_ascii_lowercase();
    let guidance_lc = guidance.unwrap_or_default().to_ascii_lowercase();
    if !hint.contains("multiline") {
        let collapsed = answer.split_whitespace().collect::<Vec<_>>().join(" ");
        if collapsed != answer {
            answer = collapsed;
            notes.push("collapsed whitespace".to_string());
        }
    }

    if hint.contains("yes_no")
        || guidance_lc.contains("yes/no")
        || guidance_lc.contains("yes or no")
    {
        let lower = answer.to_ascii_lowercase();
        let first_token = lower
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .trim_matches(|ch: char| !ch.is_ascii_alphanumeric());
        if lower == "yes" || first_token == "yes" {
            answer = "yes".to_string();
            notes.push("normalized yes/no answer".to_string());
        } else if lower == "no" || first_token == "no" {
            answer = "no".to_string();
            notes.push("normalized yes/no answer".to_string());
        }
    }

    if hint.contains("comma") || guidance_lc.contains("comma-separated") {
        let normalized = answer
            .split(',')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(",");
        if normalized != answer {
            answer = normalized;
            notes.push("normalized comma-separated spacing".to_string());
        }
    }

    let requested_places = requested_decimal_places(&guidance_lc).or_else(|| {
        if hint.contains("numeric") {
            requested_decimal_places(&hint)
        } else {
            None
        }
    });
    if let Some(places) = requested_places
        && is_plain_number(&answer)
        && let Ok(value) = answer.replace(',', "").parse::<f64>()
    {
        answer = format!("{value:.places$}");
        notes.push(format!("rounded numeric answer to {places} decimal places"));
    }

    (answer, notes)
}

fn strip_code_fence(value: &str) -> String {
    if !value.starts_with("```") {
        return value.to_string();
    }
    let mut lines = value.lines();
    let Some(first) = lines.next() else {
        return value.to_string();
    };
    if !first.starts_with("```") {
        return value.to_string();
    }
    let mut inner: Vec<&str> = lines.collect();
    if inner
        .last()
        .is_some_and(|line| line.trim_start().starts_with("```"))
    {
        inner.pop();
        inner.join("\n")
    } else {
        value.to_string()
    }
}

fn requested_decimal_places(text: &str) -> Option<usize> {
    for pattern in [
        r"round(?:ed)?\s+to\s+(\d+)\s+decimal",
        r"(\d+)\s+decimal\s+places?",
    ] {
        let re = regex::Regex::new(pattern).ok()?;
        if let Some(caps) = re.captures(text)
            && let Some(m) = caps.get(1)
            && let Ok(value) = m.as_str().parse::<usize>()
        {
            return Some(value.min(12));
        }
    }
    None
}

fn is_plain_number(value: &str) -> bool {
    regex::Regex::new(r"^-?\d+(?:,\d{3})*(?:\.\d+)?$|^-?\d+(?:\.\d+)?$")
        .map(|re| re.is_match(value.trim()))
        .unwrap_or(false)
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
    approval_mode: ApprovalMode,
    pending_approvals: PendingWriteApprovals,
}

impl WriteFileTool {
    pub fn new(
        service: Arc<FileSystemService>,
        approval_mode: ApprovalMode,
        pending_approvals: PendingWriteApprovals,
    ) -> Self {
        Self {
            service,
            approval_mode,
            pending_approvals,
        }
    }
}

impl Tool for WriteFileTool {
    const NAME: &'static str = "write_file";
    type Error = ToolError;
    type Args = WriteFileArgs;
    type Output = WriteFileOutput;

    fn description(&self) -> String {
        "Create or overwrite a file within the workspace. \
                         Requires user confirmation before writing. \
                         The file path must be within the workspace directory.\n\
                         \n\
                         Examples:\n\
                         - Create new file: {\"path\": \"src/utils.rs\", \"content\": \"pub fn hello() {}\"}\n\
                         - Write config: {\"path\": \"config.json\", \"content\": \"{\\\"key\\\": \\\"value\\\"}\"}"
                .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
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
        // Check if file exists to determine if this is an overwrite
        let is_overwrite = self.service.read_file(&args.path).await.is_ok();

        let operation = WriteOperation::WriteFile {
            path: args.path.clone(),
            is_overwrite,
            content_preview: preview(&args.content, PREVIEW_MAX_CHARS),
        };

        let approved =
            request_write_approval(&self.pending_approvals, &self.approval_mode, operation).await?;
        if !approved {
            return Err(ToolError::OperationFailed(
                "Write operation denied by user".to_string(),
            ));
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
    type Error = ToolError;
    type Args = CreateDirectoryArgs;
    type Output = CreateDirectoryOutput;

    fn description(&self) -> String {
        "Create a directory (and any necessary parent directories) within the workspace. \
                         Does not require confirmation as it is non-destructive.\n\
                         \n\
                         Examples:\n\
                         - Create directory: {\"path\": \"src/components\"}\n\
                         - Create nested: {\"path\": \"tests/integration/fixtures\"}"
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the directory to create, relative to the workspace root"
                }
            },
            "required": ["path"]
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
    approval_mode: ApprovalMode,
    pending_approvals: PendingWriteApprovals,
}

impl DeleteFileTool {
    pub fn new(
        service: Arc<FileSystemService>,
        approval_mode: ApprovalMode,
        pending_approvals: PendingWriteApprovals,
    ) -> Self {
        Self {
            service,
            approval_mode,
            pending_approvals,
        }
    }
}

impl Tool for DeleteFileTool {
    const NAME: &'static str = "delete_file";
    type Error = ToolError;
    type Args = DeleteFileArgs;
    type Output = DeleteFileOutput;

    fn description(&self) -> String {
        "Delete a file within the workspace. \
                         Requires user confirmation before deleting. \
                         This operation is irreversible.\n\
                         \n\
                         Examples:\n\
                         - Delete file: {\"path\": \"temp/output.log\"}\n\
                         - Remove old config: {\"path\": \"config.old.json\"}"
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to delete, relative to the workspace root"
                }
            },
            "required": ["path"]
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
        let operation = WriteOperation::DeleteFile {
            path: args.path.clone(),
        };

        let approved =
            request_write_approval(&self.pending_approvals, &self.approval_mode, operation).await?;
        if !approved {
            return Err(ToolError::OperationFailed(
                "Delete operation denied by user".to_string(),
            ));
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
    approval_mode: ApprovalMode,
    pending_approvals: PendingWriteApprovals,
}

impl MoveFileTool {
    pub fn new(
        service: Arc<FileSystemService>,
        approval_mode: ApprovalMode,
        pending_approvals: PendingWriteApprovals,
    ) -> Self {
        Self {
            service,
            approval_mode,
            pending_approvals,
        }
    }
}

impl Tool for MoveFileTool {
    const NAME: &'static str = "move_file";
    type Error = ToolError;
    type Args = MoveFileArgs;
    type Output = MoveFileOutput;

    fn description(&self) -> String {
        "Move or rename a file within the workspace. \
                         Requires user confirmation. \
                         The destination must not already exist.\n\
                         \n\
                         Examples:\n\
                         - Rename: {\"source\": \"old_name.rs\", \"destination\": \"new_name.rs\"}\n\
                         - Move: {\"source\": \"file.txt\", \"destination\": \"archive/file.txt\"}"
                .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
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
        let operation = WriteOperation::MoveFile {
            source: args.source.clone(),
            destination: args.destination.clone(),
        };

        let approved =
            request_write_approval(&self.pending_approvals, &self.approval_mode, operation).await?;
        if !approved {
            return Err(ToolError::OperationFailed(
                "Move operation denied by user".to_string(),
            ));
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

#[cfg(test)]
mod tests {
    use super::{normalize_final_answer, preview, request_write_approval};
    use crate::models::write_approval_store::{
        WriteApprovalDecision, WriteApprovalStore, WriteOperation,
    };
    use crate::settings::models::execution_settings::ApprovalMode;

    /// AGE-246 / D7: two agents, each with its own `WriteApprovalStore`, must
    /// not cross-notify — a request made against agent A's store is
    /// delivered only to A's receiver, never B's.
    #[tokio::test]
    async fn write_approval_notifies_only_the_owning_store() {
        let mut store_a = WriteApprovalStore::new();
        let mut store_b = WriteApprovalStore::new();

        let (tx_a, mut rx_a) = tokio::sync::mpsc::unbounded_channel();
        store_a.set_notifier(tx_a);
        let (tx_b, mut rx_b) = tokio::sync::mpsc::unbounded_channel();
        store_b.set_notifier(tx_b);

        let pending_a = store_a.get_pending_approvals();
        let waiter = tokio::spawn({
            let pending_a = pending_a.clone();
            async move {
                request_write_approval(
                    &pending_a,
                    &ApprovalMode::AlwaysAsk,
                    WriteOperation::DeleteFile {
                        path: "/tmp/x".to_string(),
                    },
                )
                .await
            }
        });

        let notification = rx_a
            .recv()
            .await
            .expect("agent A's receiver must see the notification");
        assert!(
            rx_b.try_recv().is_err(),
            "agent B's receiver must not see agent A's request"
        );

        assert!(store_a.resolve(&notification.id, WriteApprovalDecision::Approved));
        assert!(waiter.await.unwrap().unwrap());
    }

    /// AGE-193: the write-approval policy is the agent's own, not a process
    /// global. Two agents streaming concurrently under different modes must
    /// each get their own behaviour — B's auto-approve must not leak into A's
    /// always-ask, and A's request must still reach only A's notifier.
    #[tokio::test]
    async fn concurrent_agents_keep_their_own_write_approval_mode() {
        let mut store_a = WriteApprovalStore::new();
        let mut store_b = WriteApprovalStore::new();

        let (tx_a, mut rx_a) = tokio::sync::mpsc::unbounded_channel();
        store_a.set_notifier(tx_a);
        let (tx_b, mut rx_b) = tokio::sync::mpsc::unbounded_channel();
        store_b.set_notifier(tx_b);

        let op = || WriteOperation::DeleteFile {
            path: "/tmp/x".to_string(),
        };

        let pending_a = store_a.get_pending_approvals();
        let waiter_a = tokio::spawn({
            let pending_a = pending_a.clone();
            async move { request_write_approval(&pending_a, &ApprovalMode::AlwaysAsk, op()).await }
        });
        let pending_b = store_b.get_pending_approvals();
        let waiter_b = tokio::spawn({
            let pending_b = pending_b.clone();
            async move { request_write_approval(&pending_b, &ApprovalMode::AutoApproveAll, op()).await }
        });

        // B auto-approves without ever prompting.
        assert!(waiter_b.await.unwrap().unwrap());
        assert!(
            rx_b.try_recv().is_err(),
            "auto-approve must not post a notification"
        );

        // A is still waiting on its own prompt, untouched by B's policy.
        let notification = rx_a
            .recv()
            .await
            .expect("agent A's receiver must see the notification");
        assert!(
            !waiter_a.is_finished(),
            "always-ask must block until resolved"
        );
        assert!(store_a.resolve(&notification.id, WriteApprovalDecision::Denied));
        assert!(!waiter_a.await.unwrap().unwrap());
    }

    #[test]
    fn preview_truncates_on_char_boundary_not_bytes() {
        // '─' is 3 bytes; a 200-byte cut can land inside it and panic with byte indexing.
        let content = "─".repeat(100);
        let result = preview(&content, 50);
        assert_eq!(result.chars().count(), 53); // 50 + "..."
        assert!(result.ends_with("..."));
    }

    #[test]
    fn preview_short_string_unchanged() {
        assert_eq!(preview("hello", 200), "hello");
    }

    #[test]
    fn final_answer_normalizes_scalar_wrappers() {
        let (answer, notes) = normalize_final_answer("```text\n  NexPay:482.08  \n```", None, None);

        assert_eq!(answer, "NexPay:482.08");
        assert!(!notes.iter().any(|note| note.contains("rounded")));
    }

    #[test]
    fn final_answer_applies_decimal_guidance() {
        let (answer, notes) =
            normalize_final_answer("482.0849", Some("rounded to 2 decimal places"), None);

        assert_eq!(answer, "482.08");
        assert!(
            notes
                .iter()
                .any(|note| note == "rounded numeric answer to 2 decimal places")
        );
    }

    #[test]
    fn final_answer_preserves_structured_non_numeric_answer() {
        let (answer, notes) =
            normalize_final_answer("NexPay:482.0849", Some("rounded to 2 decimal places"), None);

        assert_eq!(answer, "NexPay:482.0849");
        assert!(
            !notes
                .iter()
                .any(|note| note.contains("rounded numeric answer"))
        );
    }

    #[test]
    fn final_answer_normalizes_yes_no_and_commas() {
        let (answer, _) = normalize_final_answer("Yes, because it matches", Some("yes/no"), None);
        assert_eq!(answer, "yes");

        let (answer, _) = normalize_final_answer("A, B, C", None, Some("comma_separated"));
        assert_eq!(answer, "A,B,C");
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
    approval_mode: ApprovalMode,
    pending_approvals: PendingWriteApprovals,
}

impl ApplyDiffTool {
    pub fn new(
        service: Arc<FileSystemService>,
        approval_mode: ApprovalMode,
        pending_approvals: PendingWriteApprovals,
    ) -> Self {
        Self {
            service,
            approval_mode,
            pending_approvals,
        }
    }
}

impl Tool for ApplyDiffTool {
    const NAME: &'static str = "apply_diff";
    type Error = ToolError;
    type Args = ApplyDiffArgs;
    type Output = ApplyDiffOutput;

    fn description(&self) -> String {
        "Apply a targeted edit to a file by replacing specific content. \
                         Requires user confirmation. \
                         Provide the exact old content to find and the new content to replace it with. \
                         Only the first occurrence is replaced.\n\
                         \n\
                         Examples:\n\
                         - Fix typo: {\"path\": \"README.md\", \"old_content\": \"teh\", \"new_content\": \"the\"}\n\
                         - Update function: {\"path\": \"src/main.rs\", \"old_content\": \"fn old()\", \"new_content\": \"fn new()\"}"
                .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
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
        let operation = WriteOperation::ApplyDiff {
            path: args.path.clone(),
            old_preview: preview(&args.old_content, PREVIEW_MAX_CHARS),
            new_preview: preview(&args.new_content, PREVIEW_MAX_CHARS),
        };

        let approved =
            request_write_approval(&self.pending_approvals, &self.approval_mode, operation).await?;
        if !approved {
            return Err(ToolError::OperationFailed(
                "Diff operation denied by user".to_string(),
            ));
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
