use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::chatty::services::filesystem_service::FileSystemService;
use crate::chatty::views::attachment_validation::validate_attachment;

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
        validate_attachment(&canonical).map_err(|e| {
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
        let file_type = if ext == "pdf" {
            "pdf".to_string()
        } else {
            "image".to_string()
        };

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
