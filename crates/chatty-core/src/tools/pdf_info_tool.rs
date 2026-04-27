use pdfium_render::prelude::*;
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::services::filesystem_service::FileSystemService;
use crate::services::pdfium_utils::create_pdfium;
use crate::tools::ToolError;

#[derive(Deserialize, Serialize)]
pub struct PdfInfoArgs {
    pub path: String,
}

#[derive(Debug, Serialize)]
pub struct PageInfo {
    pub index: u32,
    pub width_pt: f32,
    pub height_pt: f32,
    pub width_in: f32,
    pub height_in: f32,
    pub label: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct PdfInfoOutput {
    pub path: String,
    pub page_count: u32,
    pub title: Option<String>,
    pub author: Option<String>,
    pub subject: Option<String>,
    pub creator: Option<String>,
    pub producer: Option<String>,
    pub creation_date: Option<String>,
    pub modification_date: Option<String>,
    pub pages: Vec<PageInfo>,
}

#[derive(Clone)]
pub struct PdfInfoTool {
    service: Arc<FileSystemService>,
}

impl PdfInfoTool {
    pub fn new(service: Arc<FileSystemService>) -> Self {
        Self { service }
    }
}

/// Maximum number of pages to include in the `pages` array returned by `pdf_info`.
/// For PDFs with more pages, only the first `MAX_INFO_PAGES` entries are returned;
/// `page_count` still reflects the true total.
const MAX_INFO_PAGES: u32 = 100;

impl Tool for PdfInfoTool {
    const NAME: &'static str = "pdf_info";
    type Error = ToolError;
    type Args = PdfInfoArgs;
    type Output = PdfInfoOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "pdf_info".to_string(),
            description: "Get metadata and structural information about a PDF file. \
                         Returns page count, page dimensions, and document metadata \
                         (title, author, creation date, etc.). Use this to understand \
                         a PDF's structure before converting pages or extracting text.\n\
                         \n\
                         Page dimension details are returned for up to 100 pages; \
                         page_count always reflects the true total.\n\
                         \n\
                         Examples:\n\
                         - Get PDF info: {\"path\": \"docs/report.pdf\"}\n\
                         - Check page count: {\"path\": \"scans/document.pdf\"}"
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the PDF file, relative to the workspace root or absolute within workspace"
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
            return Err(ToolError::OperationFailed(format!(
                "File '{}' is not a PDF (extension: {})",
                args.path, ext
            )));
        }

        let pdf_path = canonical.clone();
        let result = tokio::task::spawn_blocking(move || get_pdf_info(&pdf_path))
            .await
            .map_err(|e| ToolError::OperationFailed(format!("Task join error: {}", e)))??;

        Ok(PdfInfoOutput {
            path: args.path,
            page_count: result.page_count,
            title: result.title,
            author: result.author,
            subject: result.subject,
            creator: result.creator,
            producer: result.producer,
            creation_date: result.creation_date,
            modification_date: result.modification_date,
            pages: result.pages,
        })
    }
}

struct PdfInfoResult {
    page_count: u32,
    title: Option<String>,
    author: Option<String>,
    subject: Option<String>,
    creator: Option<String>,
    producer: Option<String>,
    creation_date: Option<String>,
    modification_date: Option<String>,
    pages: Vec<PageInfo>,
}

fn get_pdf_info(pdf_path: &std::path::Path) -> Result<PdfInfoResult, ToolError> {
    let pdfium = create_pdfium()?;
    let document = pdfium.load_pdf_from_file(pdf_path, None).map_err(|e| {
        ToolError::OperationFailed(format!(
            "Failed to open PDF '{}': {:?}",
            pdf_path.display(),
            e
        ))
    })?;

    let metadata = document.metadata();
    let get_tag = |tag_type: PdfDocumentMetadataTagType| -> Option<String> {
        metadata.get(tag_type).map(|t| t.value().to_string())
    };

    let page_count = document.pages().len() as u32;
    let mut pages = Vec::new();

    for i in 0..page_count.min(MAX_INFO_PAGES) {
        if let Ok(page) = document.pages().get(i as i32) {
            let width_pt = page.width().value;
            let height_pt = page.height().value;
            pages.push(PageInfo {
                index: i,
                width_pt,
                height_pt,
                width_in: width_pt / 72.0,
                height_in: height_pt / 72.0,
                label: page.label().map(|s| s.to_string()),
            });
        }
    }

    Ok(PdfInfoResult {
        page_count,
        title: get_tag(PdfDocumentMetadataTagType::Title),
        author: get_tag(PdfDocumentMetadataTagType::Author),
        subject: get_tag(PdfDocumentMetadataTagType::Subject),
        creator: get_tag(PdfDocumentMetadataTagType::Creator),
        producer: get_tag(PdfDocumentMetadataTagType::Producer),
        creation_date: get_tag(PdfDocumentMetadataTagType::CreationDate),
        modification_date: get_tag(PdfDocumentMetadataTagType::ModificationDate),
        pages,
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

    async fn create_test_tool() -> (PdfInfoTool, PathBuf) {
        let workspace = std::env::temp_dir().join("chatty_pdf_info_tests");
        let _ = fs::create_dir_all(&workspace);
        let service = Arc::new(
            FileSystemService::new(workspace.to_str().unwrap())
                .await
                .unwrap(),
        );
        let tool = PdfInfoTool::new(service);
        (tool, workspace)
    }

    #[tokio::test]
    async fn test_definition_metadata() {
        let (tool, _) = create_test_tool().await;
        let def = tool.definition("test".into()).await;

        assert_eq!(def.name, "pdf_info");
        assert!(def.description.contains("metadata"));
        assert_eq!(def.parameters["required"][0], "path");
    }

    #[tokio::test]
    async fn test_get_info_valid_pdf() {
        let (tool, workspace) = create_test_tool().await;
        let pdf_path = workspace.join("test_info.pdf");
        create_test_pdf(&pdf_path);

        let result = tool
            .call(PdfInfoArgs {
                path: "test_info.pdf".into(),
            })
            .await;

        let _ = fs::remove_file(&pdf_path);

        assert!(result.is_ok(), "Expected success, got: {:?}", result.err());
        let output = result.unwrap();
        assert_eq!(output.page_count, 1);
        assert_eq!(output.pages.len(), 1);
        assert!((output.pages[0].width_pt - 612.0).abs() < 1.0);
        assert!((output.pages[0].height_pt - 792.0).abs() < 1.0);
    }

    #[tokio::test]
    async fn test_rejects_non_pdf() {
        let (tool, workspace) = create_test_tool().await;
        let txt_path = workspace.join("notes.txt");
        fs::write(&txt_path, "hello").unwrap();

        let result = tool
            .call(PdfInfoArgs {
                path: "notes.txt".into(),
            })
            .await;

        let _ = fs::remove_file(&txt_path);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_rejects_nonexistent() {
        let (tool, _) = create_test_tool().await;
        let result = tool
            .call(PdfInfoArgs {
                path: "nonexistent.pdf".into(),
            })
            .await;
        assert!(result.is_err());
    }
}
