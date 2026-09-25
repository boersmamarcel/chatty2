#[cfg(test)]
use rig_agent::tool::tool_definition;
use rig_agent::tool::{Tool, ToolContext, ToolExecutionError};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::services::filesystem_service::FileSystemService;
use crate::services::pdfium_utils::create_pdfium;
use crate::tools::ToolError;

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
    type Error = ToolError;
    type Args = PdfExtractTextArgs;
    type Output = PdfExtractTextOutput;

    fn description(&self) -> String {
        "Extract text content from PDF pages. Returns the raw text from \
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
                .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
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

        let pages_arg = args.pages.clone();
        let pdf_path = canonical.clone();
        let result =
            tokio::task::spawn_blocking(move || extract_text(&pdf_path, pages_arg.as_deref()))
                .await
                .map_err(|e| ToolError::OperationFailed(format!("Task join error: {}", e)))??;

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

/// The page indices to read out of `total_pages`: the requested ones that
/// exist, or the first [`MAX_PAGES`].
fn page_indices(pages: Option<&[u32]>, total_pages: u32) -> Vec<u32> {
    match pages {
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
    }
}

/// Text of the requested pages, through pdfium when it binds and through
/// the pure-Rust `lopdf` reader when it does not — Linux task containers
/// ship no pdfium, and the model used to detour through `pip install
/// pypdf` after "Failed to bind pdfium".
fn extract_text(
    pdf_path: &std::path::Path,
    pages: Option<&[u32]>,
) -> Result<ExtractResult, ToolError> {
    let pdfium = match create_pdfium() {
        Ok(pdfium) => pdfium,
        Err(bind_error) => {
            tracing::warn!(error = %bind_error, "pdfium unavailable; extracting PDF text with lopdf");
            return extract_text_without_pdfium(pdf_path, pages).map_err(|fallback_error| {
                ToolError::OperationFailed(format!(
                    "Could not read '{}': the pdfium library is not available here and the \
                     built-in reader failed ({fallback_error}). Try `pdftotext <file> -` in the \
                     shell instead.",
                    pdf_path.display()
                ))
            });
        }
    };
    let document = pdfium.load_pdf_from_file(pdf_path, None).map_err(|e| {
        ToolError::OperationFailed(format!(
            "Failed to open PDF '{}': {:?}",
            pdf_path.display(),
            e
        ))
    })?;

    let total_pages = document.pages().len() as u32;

    let page_indices = page_indices(pages, total_pages);

    let mut result_pages = Vec::new();

    for &page_idx in &page_indices {
        let page = document.pages().get(page_idx as i32).map_err(|e| {
            ToolError::OperationFailed(format!("Failed to get page {}: {:?}", page_idx, e))
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

/// [`extract_text`] with `lopdf` alone. Its text comes from the content
/// streams' show-text operators, so layout (columns, table cells) is looser
/// than pdfium's.
fn extract_text_without_pdfium(
    pdf_path: &std::path::Path,
    pages: Option<&[u32]>,
) -> Result<ExtractResult, String> {
    // lopdf indexes into the file's own offsets and lengths; a malformed
    // PDF can make it panic. Turn that into a readable failure here rather
    // than a bare "task panicked" from the blocking task.
    let result = std::panic::catch_unwind(|| {
        let document = lopdf::Document::load(pdf_path).map_err(|e| e.to_string())?;
        let total_pages = document.get_pages().len() as u32;
        let pages: Vec<PageText> = page_indices(pages, total_pages)
            .into_iter()
            .map(|page| {
                // lopdf numbers pages from 1; a page it can't decode reads
                // as empty, like pdfium's.
                let text = document.extract_text(&[page + 1]).unwrap_or_default();
                PageText { page, text }
            })
            .collect();
        Ok(ExtractResult { total_pages, pages })
    })
    .unwrap_or_else(|_| Err("the PDF is malformed".to_string()))?;
    // Scanned pages, or fonts without a Unicode map, give lopdf nothing to
    // read. Empty pages would read as "this PDF says nothing"; say it could
    // not be read instead, so the error points at pdftotext / OCR.
    if !result.pages.is_empty() && result.pages.iter().all(|p| p.text.trim().is_empty()) {
        return Err(
            "no text could be read from the requested pages (scanned, or fonts \
                    without a text encoding)"
                .to_string(),
        );
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig_agent::tool::{Tool, ToolContext};
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
        let def = tool_definition(&tool);

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
            .call(
                &mut ToolContext::new(),
                PdfExtractTextArgs {
                    path: "test_extract.pdf".into(),
                    pages: None,
                },
            )
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
            .call(
                &mut ToolContext::new(),
                PdfExtractTextArgs {
                    path: "test_specific_text.pdf".into(),
                    pages: Some(vec![0]),
                },
            )
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
            .call(
                &mut ToolContext::new(),
                PdfExtractTextArgs {
                    path: "notes.txt".into(),
                    pages: None,
                },
            )
            .await;

        let _ = fs::remove_file(&txt_path);
        assert!(result.is_err());
    }

    /// A two-page PDF with correct xref offsets, one line of text per page.
    fn two_page_pdf() -> Vec<u8> {
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 5 0 R \
             /Resources << /Font << /F1 7 0 R >> >> >>"
                .to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 6 0 R \
             /Resources << /Font << /F1 7 0 R >> >> >>"
                .to_string(),
            stream("BT /F1 12 Tf 72 700 Td (Revenue grew 12 percent) Tj ET"),
            stream("BT /F1 12 Tf 72 700 Td (Second page text) Tj ET"),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
        ];
        let mut pdf = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, object) in objects.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", i + 1).as_bytes());
        }
        let xref = pdf.len();
        pdf.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
        );
        for offset in offsets {
            pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        pdf
    }

    fn stream(content: &str) -> String {
        format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        )
    }

    #[test]
    fn test_extracts_text_without_pdfium() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("report.pdf");
        fs::write(&path, two_page_pdf()).unwrap();

        let all = extract_text_without_pdfium(&path, None).unwrap();
        assert_eq!(all.total_pages, 2);
        assert!(all.pages[0].text.contains("Revenue grew 12 percent"));
        assert!(all.pages[1].text.contains("Second page text"));

        let second = extract_text_without_pdfium(&path, Some(&[1, 9])).unwrap();
        assert_eq!(second.pages.len(), 1);
        assert_eq!(second.pages[0].page, 1);
        assert!(second.pages[0].text.contains("Second page"));
    }

    #[test]
    fn test_fallback_reports_an_unreadable_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broken.pdf");
        fs::write(&path, b"not a pdf").unwrap();
        assert!(extract_text_without_pdfium(&path, None).is_err());
    }

    /// Pages with no show-text operators come back as an error that says
    /// so, not as empty pages.
    #[test]
    fn test_fallback_reports_pages_without_text() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("scan.pdf");
        let mut pdf = String::from_utf8(two_page_pdf()).unwrap();
        for shown in ["(Revenue grew 12 percent) Tj", "(Second page text) Tj"] {
            // Same length, so the xref offsets stay right.
            pdf = pdf.replace(shown, &" ".repeat(shown.len()));
        }
        fs::write(&path, pdf).unwrap();
        let Err(error) = extract_text_without_pdfium(&path, None) else {
            panic!("pages without text must be an error");
        };
        assert!(error.contains("no text"), "{error}");
    }

    /// Truncated and garbled inputs fail with an error, never a panic.
    #[test]
    fn test_fallback_survives_truncated_pdfs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cut.pdf");
        let pdf = two_page_pdf();
        for cut in (0..pdf.len()).step_by(37) {
            fs::write(&path, &pdf[..cut]).unwrap();
            let _ = extract_text_without_pdfium(&path, None);
        }
    }

    #[tokio::test]
    async fn test_rejects_nonexistent() {
        let (tool, _) = create_test_tool().await;
        let result = tool
            .call(
                &mut ToolContext::new(),
                PdfExtractTextArgs {
                    path: "nonexistent.pdf".into(),
                    pages: None,
                },
            )
            .await;
        assert!(result.is_err());
    }
}
