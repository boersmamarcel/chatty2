use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::chatty::services::filesystem_service::FileSystemService;
use crate::chatty::services::pdfium_utils::create_pdfium;

#[derive(Debug, thiserror::Error)]
pub enum PdfExtractTextError {
    #[error("PDF text extraction error: {0}")]
    OperationError(#[from] anyhow::Error),
}

#[derive(Deserialize, Serialize)]
pub struct PdfExtractTextArgs {
    pub path: String,
    #[serde(default)]
    pub pages: Option<Vec<u32>>,
}

#[derive(Debug, Serialize)]
pub struct PageText {
    pub page: u32,
    pub text: String,
}

#[derive(Debug, Serialize)]
pub struct PdfExtractTextOutput {
    pub path: String,
    pub total_pages: u32,
    pub extracted_pages: u32,
    pub pages: Vec<PageText>,
}

#[derive(Clone)]
pub struct PdfExtractTextTool {
    service: Arc<FileSystemService>,
}

impl PdfExtractTextTool {
    pub fn new(service: Arc<FileSystemService>) -> Self {
        Self { service }
    }
}

/// Maximum number of pages to extract text from in a single call
const MAX_PAGES: usize = 50;

impl Tool for PdfExtractTextTool {
    const NAME: &'static str = "pdf_extract_text";
    type Error = PdfExtractTextError;
    type Args = PdfExtractTextArgs;
    type Output = PdfExtractTextOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "pdf_extract_text".to_string(),
            description: "Extract text content from PDF pages. Returns the raw text from \
                         specified pages (or all pages) of a PDF. Use this to read PDF \
                         documents, search for content, or process text from scanned documents \
                         that have OCR layers.\n\
                         \n\
                         Maximum 50 pages per call. Note: scanned PDFs without OCR layers \
                         may return empty text.\n\
                         \n\
                         Examples:\n\
                         - Extract all text: {\"path\": \"docs/report.pdf\"}\n\
                         - Extract specific pages: {\"path\": \"docs/report.pdf\", \"pages\": [0, 1, 2]}"
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
                        "description": "Zero-indexed page numbers to extract text from. If omitted, extracts from all pages (up to 50)."
                    }
                },
                "required": ["path"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let canonical = self.service.resolve_path(&args.path).await?;

        let ext = canonical
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if ext != "pdf" {
            return Err(PdfExtractTextError::OperationError(anyhow::anyhow!(
                "File '{}' is not a PDF (extension: {})",
                args.path,
                ext
            )));
        }

        let pages_arg = args.pages.clone();
        let pdf_path = canonical.clone();
        let result =
            tokio::task::spawn_blocking(move || extract_text(&pdf_path, pages_arg.as_deref()))
                .await
                .map_err(|e| {
                    PdfExtractTextError::OperationError(anyhow::anyhow!("Task join error: {}", e))
                })??;

        Ok(PdfExtractTextOutput {
            path: args.path,
            total_pages: result.total_pages,
            extracted_pages: result.pages.len() as u32,
            pages: result.pages,
        })
    }
}

struct ExtractResult {
    total_pages: u32,
    pages: Vec<PageText>,
}

fn extract_text(
    pdf_path: &std::path::Path,
    pages: Option<&[u32]>,
) -> Result<ExtractResult, PdfExtractTextError> {
    let pdfium = create_pdfium()?;
    let document = pdfium.load_pdf_from_file(pdf_path, None).map_err(|e| {
        PdfExtractTextError::OperationError(anyhow::anyhow!(
            "Failed to open PDF '{}': {:?}",
            pdf_path.display(),
            e
        ))
    })?;

    let total_pages = document.pages().len() as u32;

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

    let mut result_pages = Vec::new();

    for &page_idx in &page_indices {
        let page = document.pages().get(page_idx as u16).map_err(|e| {
            PdfExtractTextError::OperationError(anyhow::anyhow!(
                "Failed to get page {}: {:?}",
                page_idx,
                e
            ))
        })?;

        let text = page.text().map(|t| t.all()).unwrap_or_default();

        result_pages.push(PageText {
            page: page_idx,
            text,
        });
    }

    Ok(ExtractResult {
        total_pages,
        pages: result_pages,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig::tool::Tool;
    use std::fs;
    use std::io::Write;
    use std::path::PathBuf;

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

    async fn create_test_tool() -> (PdfExtractTextTool, PathBuf) {
        let workspace = std::env::temp_dir().join("chatty_pdf_extract_text_tests");
        let _ = fs::create_dir_all(&workspace);
        let service = Arc::new(
            FileSystemService::new(workspace.to_str().unwrap())
                .await
                .unwrap(),
        );
        let tool = PdfExtractTextTool::new(service);
        (tool, workspace)
    }

    #[tokio::test]
    async fn test_definition_metadata() {
        let (tool, _) = create_test_tool().await;
        let def = tool.definition("test".into()).await;

        assert_eq!(def.name, "pdf_extract_text");
        assert!(def.description.contains("Extract text"));
        assert_eq!(def.parameters["required"][0], "path");
    }

    #[tokio::test]
    async fn test_extract_text_valid_pdf() {
        let (tool, workspace) = create_test_tool().await;
        let pdf_path = workspace.join("test_extract.pdf");
        create_test_pdf(&pdf_path);

        let result = tool
            .call(PdfExtractTextArgs {
                path: "test_extract.pdf".into(),
                pages: None,
            })
            .await;

        let _ = fs::remove_file(&pdf_path);

        assert!(result.is_ok(), "Expected success, got: {:?}", result.err());
        let output = result.unwrap();
        assert_eq!(output.total_pages, 1);
        assert_eq!(output.extracted_pages, 1);
        assert_eq!(output.pages.len(), 1);
    }

    #[tokio::test]
    async fn test_extract_specific_pages() {
        let (tool, workspace) = create_test_tool().await;
        let pdf_path = workspace.join("test_specific_text.pdf");
        create_test_pdf(&pdf_path);

        let result = tool
            .call(PdfExtractTextArgs {
                path: "test_specific_text.pdf".into(),
                pages: Some(vec![0]),
            })
            .await;

        let _ = fs::remove_file(&pdf_path);

        assert!(result.is_ok());
        let output = result.unwrap();
        assert_eq!(output.extracted_pages, 1);
    }

    #[tokio::test]
    async fn test_rejects_non_pdf() {
        let (tool, workspace) = create_test_tool().await;
        let txt_path = workspace.join("notes.txt");
        fs::write(&txt_path, "hello").unwrap();

        let result = tool
            .call(PdfExtractTextArgs {
                path: "notes.txt".into(),
                pages: None,
            })
            .await;

        let _ = fs::remove_file(&txt_path);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_rejects_nonexistent() {
        let (tool, _) = create_test_tool().await;
        let result = tool
            .call(PdfExtractTextArgs {
                path: "nonexistent.pdf".into(),
                pages: None,
            })
            .await;
        assert!(result.is_err());
    }
}
