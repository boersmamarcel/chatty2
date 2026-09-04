#[cfg(test)]
use rig_agent::tool::tool_definition;
use rig_agent::tool::{Tool, ToolContext, ToolExecutionError};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::models::attachment_validation::validate_attachment_async;
use crate::services::filesystem_service::FileSystemService;
use crate::tools::ToolError;

/// Thread-safe storage for artifact paths queued during a stream.
/// Drained after the stream completes to send as multimodal content.
pub type PendingArtifacts = Arc<Mutex<Vec<PathBuf>>>;

#[derive(Deserialize, Serialize)]
pub struct AddAttachmentArgs {
    pub path: String,
}

#[derive(Debug, Serialize)]
pub struct AddAttachmentOutput {
    pub path: String,
    pub file_type: String,
    pub message: String,
}

#[derive(Clone)]
pub struct AddAttachmentTool {
    service: Arc<FileSystemService>,
    pending_artifacts: PendingArtifacts,
}

impl AddAttachmentTool {
    pub fn new(service: Arc<FileSystemService>, pending_artifacts: PendingArtifacts) -> Self {
        Self {
            service,
            pending_artifacts,
        }
    }
}

impl Tool for AddAttachmentTool {
    const NAME: &'static str = "add_attachment";
    type Error = ToolError;
    type Args = AddAttachmentArgs;
    type Output = AddAttachmentOutput;

    fn description(&self) -> String {
        "Display an image file inline in the chat response. \
                         Use this to show generated plots, charts, or screenshots to the user. \
                         PDF documents produced by Typst or write tools appear as artifact cards \
                         in the transcript instead.\n\
                         \n\
                         Supported formats: PNG, JPG, JPEG, GIF, WebP, SVG, BMP (images).\n\
                         Maximum file size: 5MB.\n\
                         \n\
                         Note: the file is always displayed to the user, but on text-only models \
                         you cannot analyze the file contents — describe what you generated instead.\n\
                         \n\
                         Examples:\n\
                         - Show a generated plot: {\"path\": \"output/chart.png\"}\n\
                         - Display a screenshot: {\"path\": \"screenshots/page.png\"}"
                .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the image or PDF file, relative to the workspace root or absolute within workspace"
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
        // Resolve path within workspace
        let canonical = self.service.resolve_path(&args.path).await?;

        // Validate file (exists, size, extension)
        validate_attachment_async(&canonical).await.map_err(|e| {
            ToolError::OperationFailed(format!(
                "Attachment validation failed for '{}': {:?}",
                args.path, e
            ))
        })?;

        // Determine file type from extension
        let ext = canonical
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        let is_pdf = ext == "pdf";
        let file_type = if is_pdf {
            "pdf".to_string()
        } else {
            "image".to_string()
        };

        // Queue the path for inline display after the stream completes (images only;
        // PDFs use artifact cards in the typed transcript).
        if !is_pdf && let Ok(mut artifacts) = self.pending_artifacts.lock() {
            artifacts.push(canonical.clone());
        }

        Ok(AddAttachmentOutput {
            path: canonical.display().to_string(),
            file_type: file_type.clone(),
            message: if is_pdf {
                format!(
                    "File '{}' (pdf) is available in the artifact panel.",
                    args.path
                )
            } else {
                format!(
                    "File '{}' ({}) will be displayed inline in your response.",
                    args.path, file_type
                )
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig_agent::tool::{Tool, ToolContext};
    use std::fs;
    use std::path::Path;

    /// Create a tool backed by a real temp workspace
    async fn create_test_tool() -> (AddAttachmentTool, PendingArtifacts, PathBuf) {
        let workspace = std::env::temp_dir().join("chatty_add_attachment_tests");
        let _ = fs::create_dir_all(&workspace);
        let service = Arc::new(
            FileSystemService::new(workspace.to_str().unwrap())
                .await
                .unwrap(),
        );
        let pending: PendingArtifacts = Arc::new(Mutex::new(Vec::new()));
        let tool = AddAttachmentTool::new(service, pending.clone());
        (tool, pending, workspace)
    }

    fn create_test_file(dir: &Path, name: &str, size: usize) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, vec![0u8; size]).expect("create test file");
        path
    }

    // ── happy-path tests ──

    #[tokio::test]
    async fn test_call_queues_valid_image() {
        let (tool, pending, workspace) = create_test_tool().await;
        create_test_file(&workspace, "photo.png", 1024);

        let result = tool
            .call(
                &mut ToolContext::new(),
                AddAttachmentArgs {
                    path: "photo.png".into(),
                },
            )
            .await;

        assert!(result.is_ok());
        let output = result.unwrap();
        assert_eq!(output.file_type, "image");
        assert_eq!(pending.lock().unwrap().len(), 1);

        let _ = fs::remove_file(workspace.join("photo.png"));
    }

    #[tokio::test]
    async fn test_call_pdf_succeeds_without_inline_queue() {
        let (tool, pending, workspace) = create_test_tool().await;
        create_test_file(&workspace, "report.pdf", 2048);

        let result = tool
            .call(
                &mut ToolContext::new(),
                AddAttachmentArgs {
                    path: "report.pdf".into(),
                },
            )
            .await;

        assert!(result.is_ok());
        let output = result.unwrap();
        assert_eq!(output.file_type, "pdf");
        assert!(output.message.contains("artifact panel"));
        assert!(pending.lock().unwrap().is_empty());

        let _ = fs::remove_file(workspace.join("report.pdf"));
    }

    #[tokio::test]
    async fn test_call_accumulates_multiple_attachments() {
        let (tool, pending, workspace) = create_test_tool().await;
        create_test_file(&workspace, "a.png", 512);
        create_test_file(&workspace, "b.jpg", 512);
        create_test_file(&workspace, "c.pdf", 512);

        tool.call(
            &mut ToolContext::new(),
            AddAttachmentArgs {
                path: "a.png".into(),
            },
        )
        .await
        .unwrap();
        tool.call(
            &mut ToolContext::new(),
            AddAttachmentArgs {
                path: "b.jpg".into(),
            },
        )
        .await
        .unwrap();
        tool.call(
            &mut ToolContext::new(),
            AddAttachmentArgs {
                path: "c.pdf".into(),
            },
        )
        .await
        .unwrap();

        assert_eq!(pending.lock().unwrap().len(), 2);

        let _ = fs::remove_file(workspace.join("a.png"));
        let _ = fs::remove_file(workspace.join("b.jpg"));
        let _ = fs::remove_file(workspace.join("c.pdf"));
    }

    // ── validation-failure tests ──

    #[tokio::test]
    async fn test_call_rejects_nonexistent_file() {
        let (tool, pending, _workspace) = create_test_tool().await;

        let result = tool
            .call(
                &mut ToolContext::new(),
                AddAttachmentArgs {
                    path: "does_not_exist.png".into(),
                },
            )
            .await;

        assert!(result.is_err());
        assert!(pending.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_call_rejects_unsupported_extension() {
        let (tool, pending, workspace) = create_test_tool().await;
        create_test_file(&workspace, "notes.txt", 512);

        let result = tool
            .call(
                &mut ToolContext::new(),
                AddAttachmentArgs {
                    path: "notes.txt".into(),
                },
            )
            .await;

        assert!(result.is_err());
        assert!(pending.lock().unwrap().is_empty());

        let _ = fs::remove_file(workspace.join("notes.txt"));
    }

    // ── tool definition test ──

    #[tokio::test]
    async fn test_definition_metadata() {
        let (tool, _, _workspace) = create_test_tool().await;
        let def = tool_definition(&tool);

        assert_eq!(def.name, "add_attachment");
        assert!(def.description.contains("inline"));
        assert!(def.description.contains("5MB"));
        assert!(def.description.contains("text-only models"));
        assert_eq!(def.parameters["required"][0], "path");
    }

    // ── pending-artifacts drain test ──

    #[test]
    fn test_pending_artifacts_drain() {
        let pending: PendingArtifacts = Arc::new(Mutex::new(vec![
            PathBuf::from("/tmp/a.png"),
            PathBuf::from("/tmp/b.pdf"),
        ]));

        // Simulate the drain pattern used by finalize_stream
        let drained = pending
            .lock()
            .ok()
            .map(|mut v| v.drain(..).collect::<Vec<_>>())
            .filter(|v| !v.is_empty());

        assert!(drained.is_some());
        let paths = drained.unwrap();
        assert_eq!(paths.len(), 2);
        assert_eq!(paths[0], PathBuf::from("/tmp/a.png"));
        assert_eq!(paths[1], PathBuf::from("/tmp/b.pdf"));

        // After drain, the vec is empty
        assert!(pending.lock().unwrap().is_empty());
    }

    #[test]
    fn test_pending_artifacts_drain_empty_returns_none() {
        let pending: PendingArtifacts = Arc::new(Mutex::new(Vec::new()));

        let drained = pending
            .lock()
            .ok()
            .map(|mut v| v.drain(..).collect::<Vec<_>>())
            .filter(|v| !v.is_empty());

        // Empty drain should be filtered to None
        assert!(drained.is_none());
    }
}
