use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::chatty::services::filesystem_service::FileSystemService;
use crate::chatty::views::attachment_validation::validate_attachment_async;

/// Thread-safe storage for artifact paths queued during a stream.
/// Drained after the stream completes to send as multimodal content.
pub type PendingArtifacts = Arc<Mutex<Vec<PathBuf>>>;

#[derive(Debug, thiserror::Error)]
pub enum AddAttachmentError {
    #[error("Attachment error: {0}")]
    OperationError(#[from] anyhow::Error),
}

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
    supports_images: bool,
    supports_pdf: bool,
}

impl AddAttachmentTool {
    pub fn new(
        service: Arc<FileSystemService>,
        pending_artifacts: PendingArtifacts,
        supports_images: bool,
        supports_pdf: bool,
    ) -> Self {
        Self {
            service,
            pending_artifacts,
            supports_images,
            supports_pdf,
        }
    }
}

impl Tool for AddAttachmentTool {
    const NAME: &'static str = "add_attachment";
    type Error = AddAttachmentError;
    type Args = AddAttachmentArgs;
    type Output = AddAttachmentOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "add_attachment".to_string(),
            description: "Attach an image or PDF file for multimodal analysis. \
                         The file will be sent as visual/document content that you can inspect \
                         and analyze in your next response. Use this when you need to visually \
                         examine an image or read a PDF document.\n\
                         \n\
                         Supported formats: PNG, JPG, JPEG, GIF, WebP, SVG, BMP (images), PDF (documents).\n\
                         Maximum file size: 5MB.\n\
                         \n\
                         Examples:\n\
                         - Attach an image: {\"path\": \"screenshots/page.png\"}\n\
                         - Attach a PDF: {\"path\": \"reports/analysis.pdf\"}\n\
                         - Attach downloaded file: {\"path\": \"downloads/chart.jpg\"}"
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the image or PDF file, relative to the workspace root or absolute within workspace"
                    }
                },
                "required": ["path"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        // Resolve path within workspace
        let canonical = self.service.resolve_path(&args.path).await?;

        // Validate file (exists, size, extension)
        validate_attachment_async(&canonical).await.map_err(|e| {
            AddAttachmentError::OperationError(anyhow::anyhow!(
                "Attachment validation failed for '{}': {:?}",
                args.path,
                e
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

        // Reject unsupported types based on model capabilities
        if is_pdf && !self.supports_pdf {
            return Err(AddAttachmentError::OperationError(anyhow::anyhow!(
                "The current model does not support PDF attachments. \
                 Use read_file to read the PDF as text instead."
            )));
        }
        if !is_pdf && !self.supports_images {
            return Err(AddAttachmentError::OperationError(anyhow::anyhow!(
                "The current model does not support image attachments. \
                 This model is text-only and cannot analyze images."
            )));
        }

        // Queue the path for multimodal sending after the stream completes
        if let Ok(mut artifacts) = self.pending_artifacts.lock() {
            artifacts.push(canonical.clone());
        }

        Ok(AddAttachmentOutput {
            path: canonical.display().to_string(),
            file_type: file_type.clone(),
            message: format!(
                "File '{}' ({}) has been queued as an attachment. \
                 It will be sent as multimodal content for your analysis.",
                args.path, file_type
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig::tool::Tool;
    use std::fs;
    use std::path::Path;

    /// Create a tool backed by a real temp workspace
    async fn create_test_tool(
        supports_images: bool,
        supports_pdf: bool,
    ) -> (AddAttachmentTool, PendingArtifacts, PathBuf) {
        let workspace = std::env::temp_dir().join("chatty_add_attachment_tests");
        let _ = fs::create_dir_all(&workspace);
        let service = Arc::new(
            FileSystemService::new(workspace.to_str().unwrap())
                .await
                .unwrap(),
        );
        let pending: PendingArtifacts = Arc::new(Mutex::new(Vec::new()));
        let tool = AddAttachmentTool::new(service, pending.clone(), supports_images, supports_pdf);
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
        let (tool, pending, workspace) = create_test_tool(true, false).await;
        create_test_file(&workspace, "photo.png", 1024);

        let result = tool
            .call(AddAttachmentArgs {
                path: "photo.png".into(),
            })
            .await;

        assert!(result.is_ok());
        let output = result.unwrap();
        assert_eq!(output.file_type, "image");
        assert_eq!(pending.lock().unwrap().len(), 1);

        let _ = fs::remove_file(workspace.join("photo.png"));
    }

    #[tokio::test]
    async fn test_call_queues_valid_pdf() {
        let (tool, pending, workspace) = create_test_tool(true, true).await;
        create_test_file(&workspace, "report.pdf", 2048);

        let result = tool
            .call(AddAttachmentArgs {
                path: "report.pdf".into(),
            })
            .await;

        assert!(result.is_ok());
        let output = result.unwrap();
        assert_eq!(output.file_type, "pdf");
        assert_eq!(pending.lock().unwrap().len(), 1);

        let _ = fs::remove_file(workspace.join("report.pdf"));
    }

    #[tokio::test]
    async fn test_call_accumulates_multiple_attachments() {
        let (tool, pending, workspace) = create_test_tool(true, true).await;
        create_test_file(&workspace, "a.png", 512);
        create_test_file(&workspace, "b.jpg", 512);
        create_test_file(&workspace, "c.pdf", 512);

        tool.call(AddAttachmentArgs {
            path: "a.png".into(),
        })
        .await
        .unwrap();
        tool.call(AddAttachmentArgs {
            path: "b.jpg".into(),
        })
        .await
        .unwrap();
        tool.call(AddAttachmentArgs {
            path: "c.pdf".into(),
        })
        .await
        .unwrap();

        assert_eq!(pending.lock().unwrap().len(), 3);

        let _ = fs::remove_file(workspace.join("a.png"));
        let _ = fs::remove_file(workspace.join("b.jpg"));
        let _ = fs::remove_file(workspace.join("c.pdf"));
    }

    // ── validation-failure tests ──

    #[tokio::test]
    async fn test_call_rejects_nonexistent_file() {
        let (tool, pending, _workspace) = create_test_tool(true, true).await;

        let result = tool
            .call(AddAttachmentArgs {
                path: "does_not_exist.png".into(),
            })
            .await;

        assert!(result.is_err());
        assert!(pending.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_call_rejects_unsupported_extension() {
        let (tool, pending, workspace) = create_test_tool(true, true).await;
        create_test_file(&workspace, "notes.txt", 512);

        let result = tool
            .call(AddAttachmentArgs {
                path: "notes.txt".into(),
            })
            .await;

        assert!(result.is_err());
        assert!(pending.lock().unwrap().is_empty());

        let _ = fs::remove_file(workspace.join("notes.txt"));
    }

    // ── capability-rejection tests ──

    #[tokio::test]
    async fn test_call_rejects_image_when_not_supported() {
        let (tool, pending, workspace) = create_test_tool(false, true).await;
        create_test_file(&workspace, "photo.png", 1024);

        let result = tool
            .call(AddAttachmentArgs {
                path: "photo.png".into(),
            })
            .await;

        assert!(result.is_err());
        let err = format!("{}", result.unwrap_err());
        assert!(err.contains("does not support image"));
        assert!(pending.lock().unwrap().is_empty());

        let _ = fs::remove_file(workspace.join("photo.png"));
    }

    #[tokio::test]
    async fn test_call_rejects_pdf_when_not_supported() {
        let (tool, pending, workspace) = create_test_tool(true, false).await;
        create_test_file(&workspace, "doc.pdf", 1024);

        let result = tool
            .call(AddAttachmentArgs {
                path: "doc.pdf".into(),
            })
            .await;

        assert!(result.is_err());
        let err = format!("{}", result.unwrap_err());
        assert!(err.contains("does not support PDF"));
        assert!(pending.lock().unwrap().is_empty());

        let _ = fs::remove_file(workspace.join("doc.pdf"));
    }

    // ── tool definition test ──

    #[tokio::test]
    async fn test_definition_metadata() {
        let (tool, _, _workspace) = create_test_tool(true, true).await;
        let def = tool.definition("test".into()).await;

        assert_eq!(def.name, "add_attachment");
        assert!(def.description.contains("multimodal"));
        assert!(def.description.contains("5MB"));
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
