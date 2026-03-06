use pdfium_render::prelude::*;
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use tracing::warn;

use crate::chatty::services::filesystem_service::FileSystemService;
use crate::chatty::services::pdfium_utils::create_pdfium;
use crate::chatty::tools::add_attachment_tool::PendingArtifacts;

#[derive(Debug, thiserror::Error)]
pub enum PdfToImageError {
    #[error("PDF conversion error: {0}")]
    OperationError(#[from] anyhow::Error),
}

#[derive(Deserialize, Serialize)]
pub struct PdfToImageArgs {
    pub path: String,
    #[serde(default)]
    pub pages: Option<Vec<u32>>,
    #[serde(default = "default_dpi")]
    pub dpi: u32,
    /// Optional workspace-relative directory to save PNG files into.
    /// Created if it does not exist. Falls back to session temp dir when omitted.
    #[serde(default)]
    pub output_dir: Option<String>,
}

fn default_dpi() -> u32 {
    150
}

#[derive(Debug, Serialize)]
pub struct PdfToImageOutput {
    pub images: Vec<String>,
    pub page_count: u32,
    pub total_pages: u32,
    pub message: String,
}

#[derive(Clone)]
pub struct PdfToImageTool {
    service: Arc<FileSystemService>,
    pending_artifacts: PendingArtifacts,
}

impl PdfToImageTool {
    pub fn new(service: Arc<FileSystemService>, pending_artifacts: PendingArtifacts) -> Self {
        Self {
            service,
            pending_artifacts,
        }
    }
}

/// Maximum number of pages to convert in a single call
const MAX_PAGES: usize = 20;

/// Maximum DPI allowed
const MAX_DPI: u32 = 300;

impl Tool for PdfToImageTool {
    const NAME: &'static str = "pdf_to_image";
    type Error = PdfToImageError;
    type Args = PdfToImageArgs;
    type Output = PdfToImageOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "pdf_to_image".to_string(),
            description: "Convert PDF pages to PNG images and display them inline in chat. \
                         Renders specified pages (or all pages) of a PDF file as images. \
                         Use this when you need to visually inspect PDF content, show PDF pages \
                         to the user, or when the model doesn't support native PDF input.\n\
                         \n\
                         Maximum 20 pages per call. DPI range: 72-300 (default 150).\n\
                         \n\
                         Examples:\n\
                         - Convert all pages: {\"path\": \"docs/report.pdf\"}\n\
                         - Convert specific pages: {\"path\": \"docs/report.pdf\", \"pages\": [0, 1, 2]}\n\
                         - High resolution: {\"path\": \"docs/chart.pdf\", \"pages\": [0], \"dpi\": 300}"
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the PDF file, relative to the workspace root or absolute within workspace"
                    },
                    "pages": {
                        "type": "array",
                        "items": { "type": "integer" },
                        "description": "Zero-indexed page numbers to convert. If omitted, converts all pages (up to 20)."
                    },
                    "dpi": {
                        "type": "integer",
                        "description": "Resolution in DPI (72-300). Default: 150. Higher values produce larger, sharper images."
                    },
                    "output_dir": {
                        "type": "string",
                        "description": "Workspace-relative directory to save the PNG files into (e.g. \"pdf_images/\"). Created automatically if it does not exist. If omitted, images are saved to a session temp directory and are not persisted to the workspace."
                    }
                },
                "required": ["path"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let canonical = self.service.resolve_path(&args.path).await?;

        // Validate it's a PDF
        let ext = canonical
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if ext != "pdf" {
            return Err(PdfToImageError::OperationError(anyhow::anyhow!(
                "File '{}' is not a PDF (extension: {})",
                args.path,
                ext
            )));
        }

        // Clamp DPI
        let dpi = args.dpi.clamp(72, MAX_DPI);

        // Resolve output directory: workspace path if provided, else session temp dir.
        // create_directory validates workspace bounds; resolve_path then canonicalizes the
        // now-existing directory through the same validator to get the canonical path.
        let output_dir_path: PathBuf = if let Some(ref dir) = args.output_dir {
            self.service.create_directory(dir).await.map_err(|e| {
                PdfToImageError::OperationError(anyhow::anyhow!(
                    "Failed to create output directory '{}': {}",
                    dir,
                    e
                ))
            })?;
            self.service.resolve_path(dir).await?
        } else {
            crate::chatty::services::pdf_thumbnail::get_thumbnail_dir().map_err(|e| {
                PdfToImageError::OperationError(anyhow::anyhow!(
                    "Failed to create temp directory: {}",
                    e
                ))
            })?
        };

        // Render pages in a blocking task since pdfium is not async
        let pages_arg = args.pages.clone();
        let pdf_path = canonical.clone();
        let result = tokio::task::spawn_blocking(move || {
            render_pdf_pages(&pdf_path, pages_arg.as_deref(), dpi, output_dir_path)
        })
        .await
        .map_err(|e| {
            PdfToImageError::OperationError(anyhow::anyhow!("Task join error: {}", e))
        })??;

        // Queue all rendered images as pending artifacts
        match self.pending_artifacts.lock() {
            Ok(mut artifacts) => artifacts.extend(result.image_paths.iter().cloned()),
            Err(e) => warn!(
                error = ?e,
                path = %args.path,
                "Failed to lock pending_artifacts; images rendered to disk but not queued for display"
            ),
        }

        let image_strings: Vec<String> = result
            .image_paths
            .iter()
            .map(|p| p.display().to_string())
            .collect();

        Ok(PdfToImageOutput {
            page_count: image_strings.len() as u32,
            total_pages: result.total_pages,
            images: image_strings,
            message: format!(
                "Converted {} page(s) of '{}' to PNG images ({}dpi). Images will be displayed inline.{}",
                result.image_paths.len(),
                args.path,
                dpi,
                if args.output_dir.is_some() {
                    format!(
                        " Saved to workspace directory '{}'.",
                        args.output_dir.as_deref().unwrap_or("")
                    )
                } else {
                    String::new()
                }
            ),
        })
    }
}

struct RenderResult {
    image_paths: Vec<PathBuf>,
    total_pages: u32,
}

fn render_pdf_pages(
    pdf_path: &std::path::Path,
    pages: Option<&[u32]>,
    dpi: u32,
    output_dir: PathBuf,
) -> Result<RenderResult, PdfToImageError> {
    let pdfium = create_pdfium()?;
    let document = pdfium.load_pdf_from_file(pdf_path, None).map_err(|e| {
        PdfToImageError::OperationError(anyhow::anyhow!(
            "Failed to open PDF '{}': {:?}",
            pdf_path.display(),
            e
        ))
    })?;

    let total_pages = document.pages().len() as u32;

    // Determine which pages to render
    let page_indices: Vec<u32> = match pages {
        Some(requested) => {
            let mut indices: Vec<u32> = requested
                .iter()
                .copied()
                .filter(|&p| p < total_pages)
                .collect();
            indices.truncate(MAX_PAGES);
            indices
        }
        None => (0..total_pages.min(MAX_PAGES as u32)).collect(),
    };

    if page_indices.is_empty() {
        return Err(PdfToImageError::OperationError(anyhow::anyhow!(
            "No valid pages to convert. PDF has {} page(s).",
            total_pages
        )));
    }

    // Compute a hash of the PDF path for unique filenames
    let path_hash = {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(pdf_path.to_string_lossy().as_bytes());
        format!("{:x}", hasher.finalize())
    };

    // Scale factor: pdfium renders at 72 DPI by default, so scale = dpi / 72
    let scale = dpi as f32 / 72.0;

    let mut image_paths = Vec::new();

    for &page_idx in &page_indices {
        let page = document.pages().get(page_idx as u16).map_err(|e| {
            PdfToImageError::OperationError(anyhow::anyhow!(
                "Failed to get page {}: {:?}",
                page_idx,
                e
            ))
        })?;

        let width = (page.width().value * scale) as i32;
        let height = (page.height().value * scale) as i32;

        let render_config = PdfRenderConfig::new()
            .set_target_width(width)
            .set_maximum_height(height);

        let bitmap = page.render_with_config(&render_config).map_err(|e| {
            PdfToImageError::OperationError(anyhow::anyhow!(
                "Failed to render page {}: {:?}",
                page_idx,
                e
            ))
        })?;

        let image = bitmap.as_image();
        let output_path = output_dir.join(format!("pdf2img_{}_{}.png", &path_hash[..12], page_idx));

        image
            .save_with_format(&output_path, image::ImageFormat::Png)
            .map_err(|e| {
                PdfToImageError::OperationError(anyhow::anyhow!(
                    "Failed to save page {} as PNG: {:?}",
                    page_idx,
                    e
                ))
            })?;

        image_paths.push(output_path);
    }

    Ok(RenderResult {
        image_paths,
        total_pages,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig::tool::Tool;
    use std::fs;
    use std::io::Write;

    /// Create a minimal valid PDF for testing
    fn create_test_pdf(path: &std::path::Path) {
        let pdf_content = b"%PDF-1.4
1 0 obj
<<
/Type /Catalog
/Pages 2 0 R
>>
endobj
2 0 obj
<<
/Type /Pages
/Kids [3 0 R]
/Count 1
>>
endobj
3 0 obj
<<
/Type /Page
/Parent 2 0 R
/MediaBox [0 0 612 792]
/Contents 4 0 R
/Resources <<
/ProcSet [/PDF /Text]
>>
>>
endobj
4 0 obj
<<
/Length 44
>>
stream
BT
/F1 12 Tf
100 700 Td
(Test) Tj
ET
endstream
endobj
xref
0 5
0000000000 65535 f
0000000009 00000 n
0000000058 00000 n
0000000115 00000 n
0000000261 00000 n
trailer
<<
/Size 5
/Root 1 0 R
>>
startxref
354
%%EOF";

        let mut file = fs::File::create(path).expect("create test PDF");
        file.write_all(pdf_content).expect("write test PDF");
    }

    async fn create_test_tool() -> (PdfToImageTool, PendingArtifacts, PathBuf) {
        let workspace = std::env::temp_dir().join("chatty_pdf_to_image_tests");
        let _ = fs::create_dir_all(&workspace);
        let service = Arc::new(
            FileSystemService::new(workspace.to_str().unwrap())
                .await
                .unwrap(),
        );
        let pending: PendingArtifacts = Arc::new(std::sync::Mutex::new(Vec::new()));
        let tool = PdfToImageTool::new(service, pending.clone());
        (tool, pending, workspace)
    }

    #[tokio::test]
    async fn test_definition_metadata() {
        let (tool, _, _) = create_test_tool().await;
        let def = tool.definition("test".into()).await;

        assert_eq!(def.name, "pdf_to_image");
        assert!(def.description.contains("PDF pages to PNG"));
        assert!(def.description.contains("20 pages"));
        assert_eq!(def.parameters["required"][0], "path");
    }

    #[tokio::test]
    async fn test_convert_all_pages() {
        let (tool, pending, workspace) = create_test_tool().await;
        let pdf_path = workspace.join("test_convert.pdf");
        create_test_pdf(&pdf_path);

        let result = tool
            .call(PdfToImageArgs {
                path: "test_convert.pdf".into(),
                pages: None,
                dpi: 150,
                output_dir: Some("out_all".into()),
            })
            .await;

        let _ = fs::remove_file(&pdf_path);
        let _ = fs::remove_dir_all(workspace.join("out_all"));

        assert!(result.is_ok(), "Expected success, got: {:?}", result.err());
        let output = result.unwrap();
        assert_eq!(output.page_count, 1);
        assert_eq!(output.total_pages, 1);
        assert_eq!(pending.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn test_convert_specific_pages() {
        let (tool, pending, workspace) = create_test_tool().await;
        let pdf_path = workspace.join("test_specific.pdf");
        create_test_pdf(&pdf_path);

        let result = tool
            .call(PdfToImageArgs {
                path: "test_specific.pdf".into(),
                pages: Some(vec![0]),
                dpi: 72,
                output_dir: Some("out_specific".into()),
            })
            .await;

        let _ = fs::remove_file(&pdf_path);
        let _ = fs::remove_dir_all(workspace.join("out_specific"));

        assert!(result.is_ok());
        let output = result.unwrap();
        assert_eq!(output.page_count, 1);
        assert_eq!(pending.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn test_rejects_non_pdf() {
        let (tool, pending, workspace) = create_test_tool().await;
        let txt_path = workspace.join("notes.txt");
        fs::write(&txt_path, "hello").unwrap();

        let result = tool
            .call(PdfToImageArgs {
                path: "notes.txt".into(),
                pages: None,
                dpi: 150,
                output_dir: None,
            })
            .await;

        let _ = fs::remove_file(&txt_path);

        assert!(result.is_err());
        assert!(pending.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_rejects_nonexistent_file() {
        let (tool, pending, _) = create_test_tool().await;

        let result = tool
            .call(PdfToImageArgs {
                path: "nonexistent.pdf".into(),
                pages: None,
                dpi: 150,
                output_dir: None,
            })
            .await;

        assert!(result.is_err());
        assert!(pending.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_out_of_range_pages_skipped() {
        let (tool, pending, workspace) = create_test_tool().await;
        let pdf_path = workspace.join("test_range.pdf");
        create_test_pdf(&pdf_path);

        // Page 99 doesn't exist in a 1-page PDF - should be filtered out, page 0 kept
        let result = tool
            .call(PdfToImageArgs {
                path: "test_range.pdf".into(),
                pages: Some(vec![0, 99]),
                dpi: 150,
                output_dir: Some("out_range".into()),
            })
            .await;

        let _ = fs::remove_file(&pdf_path);
        let _ = fs::remove_dir_all(workspace.join("out_range"));

        assert!(result.is_ok());
        let output = result.unwrap();
        assert_eq!(output.page_count, 1);
        assert_eq!(pending.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn test_all_pages_out_of_range_errors() {
        let (tool, _, workspace) = create_test_tool().await;
        let pdf_path = workspace.join("test_allrange.pdf");
        create_test_pdf(&pdf_path);

        let result = tool
            .call(PdfToImageArgs {
                path: "test_allrange.pdf".into(),
                pages: Some(vec![99, 100]),
                dpi: 150,
                output_dir: None,
            })
            .await;

        let _ = fs::remove_file(&pdf_path);

        assert!(result.is_err());
        let err = format!("{}", result.unwrap_err());
        assert!(err.contains("No valid pages"));
    }

    #[tokio::test]
    async fn test_output_dir_saves_to_workspace() {
        let (tool, pending, workspace) = create_test_tool().await;
        let pdf_path = workspace.join("test_outdir.pdf");
        create_test_pdf(&pdf_path);

        let result = tool
            .call(PdfToImageArgs {
                path: "test_outdir.pdf".into(),
                pages: None,
                dpi: 72,
                output_dir: Some("pdf_images".into()),
            })
            .await;

        let _ = fs::remove_file(&pdf_path);

        assert!(result.is_ok(), "Expected success, got: {:?}", result.err());
        let output = result.unwrap();
        assert_eq!(output.page_count, 1);
        assert_eq!(pending.lock().unwrap().len(), 1);

        // Image should be saved inside the workspace, not in temp.
        // Canonicalize workspace to resolve symlinks (e.g. /tmp → /private/tmp on macOS).
        let img_path = PathBuf::from(&output.images[0]);
        let canonical_workspace = std::fs::canonicalize(&workspace).unwrap_or(workspace.clone());
        assert!(
            img_path.starts_with(&canonical_workspace),
            "Expected image inside workspace, got {:?}",
            img_path
        );
        assert!(img_path.exists(), "Image file should exist on disk");

        let _ = fs::remove_dir_all(workspace.join("pdf_images"));
    }
}
