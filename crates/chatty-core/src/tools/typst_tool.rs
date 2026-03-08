use super::path_utils::resolve_output_path;
use crate::services::typst_compiler_service::TypstCompilerService;
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum TypstToolError {
    #[error("Typst tool error: {0}")]
    Error(String),
}

/// Arguments for the compile_typst tool.
#[derive(Deserialize, Serialize)]
pub struct CompileTypstArgs {
    /// Typst markup source to compile. Supports headings, paragraphs, tables,
    /// math expressions, code blocks, lists, and all other Typst features.
    pub content: String,
    /// File path where the PDF will be saved. Supports absolute paths, paths
    /// starting with `~` (home directory), and relative paths resolved against
    /// the workspace directory.
    pub output_path: String,
}

/// Output returned by the compile_typst tool.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompileTypstOutput {
    /// Absolute path where the PDF was saved.
    pub saved_path: String,
    /// Number of pages in the compiled PDF.
    pub page_count: u32,
}

/// Tool that compiles Typst markup to a PDF file.
#[derive(Clone)]
pub struct CompileTypstTool {
    pub workspace_dir: Option<String>,
}

impl CompileTypstTool {
    pub fn new(workspace_dir: Option<String>) -> Self {
        Self { workspace_dir }
    }
}

impl Tool for CompileTypstTool {
    const NAME: &'static str = "compile_typst";
    type Error = TypstToolError;
    type Args = CompileTypstArgs;
    type Output = CompileTypstOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "compile_typst".to_string(),
            description: "Compile Typst markup into a PDF file and save it to disk.\n\
                \n\
                Typst is a modern document preparation system with markdown-like syntax.\n\
                Use it to produce formatted documents including:\n\
                - Reports and articles with headings, paragraphs, and lists\n\
                - Documents with native math (e.g. $ E = m c^2 $ or $ sum_(i=1)^n i $)\n\
                - Tables, figures, and code blocks\n\
                - Multi-page documents with automatic page breaks\n\
                \n\
                Typst syntax basics:\n\
                - Headings: = Heading 1, == Heading 2, === Heading 3\n\
                - Bold: *bold text*\n\
                - Italic: _italic text_\n\
                - Inline math: $ expression $\n\
                - Block math: $ expression $ on its own line\n\
                - Lists: - item or + item (numbered)\n\
                - Code: `code` or ```lang\\ncode block\\n```\n\
                - Tables: #table(columns: 2, [A], [B], [1], [2])\n\
                - Page settings: #set page(margin: (x: 2cm, y: 2cm))\n\
                - Text settings: #set text(font: \"New Computer Modern\", size: 12pt)\n\
                \n\
                The output_path can be absolute, relative to the workspace, or start with ~ for home.\n\
                Parent directories are created automatically.\n\
                \n\
                Example:\n\
                content: \"= Sales Report\\n\\n#set text(size: 11pt)\\n\\nTotal revenue: $ 1.2 times 10^6 $\\n\\n\
                #table(columns: 2, [Month], [Revenue], [Jan], [$100K], [Feb], [$120K])\"\n\
                output_path: \"reports/sales_q1.pdf\""
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "content": {
                        "type": "string",
                        "description": "Typst markup source to compile into a PDF"
                    },
                    "output_path": {
                        "type": "string",
                        "description": "File path where the PDF will be saved (absolute, relative to workspace, or starting with ~)"
                    }
                },
                "required": ["content", "output_path"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        // Resolve output path using the shared path utility (same rules as chart_tool).
        let resolved = resolve_output_path(&args.output_path, self.workspace_dir.as_deref())
            .map_err(TypstToolError::Error)?;

        // Ensure parent directory exists
        if let Some(parent) = resolved.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|e| {
                TypstToolError::Error(format!(
                    "Could not create directory '{}': {e}",
                    parent.display()
                ))
            })?;
        }

        // Compile typst source to PDF
        let base_dir = self.workspace_dir.as_deref().map(Path::new);
        let (pdf_bytes, page_count) = TypstCompilerService::compile_to_pdf(&args.content, base_dir)
            .map_err(|e| TypstToolError::Error(e.to_string()))?;

        // Write PDF to disk
        std::fs::write(&resolved, &pdf_bytes).map_err(|e| {
            TypstToolError::Error(format!(
                "Failed to write PDF to '{}': {e}",
                resolved.display()
            ))
        })?;

        Ok(CompileTypstOutput {
            saved_path: resolved.to_string_lossy().into_owned(),
            page_count,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig::tool::Tool;

    #[tokio::test]
    async fn test_definition_metadata() {
        let tool = CompileTypstTool::new(None);
        let def = tool.definition("test".into()).await;
        assert_eq!(def.name, "compile_typst");
        assert!(def.description.contains("PDF"));
        assert!(def.description.contains("Typst"));
        assert_eq!(def.parameters["required"][0], "content");
        assert_eq!(def.parameters["required"][1], "output_path");
    }

    #[tokio::test]
    async fn test_compile_to_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let output_path = dir.path().join("test.pdf").to_string_lossy().into_owned();

        let tool = CompileTypstTool::new(None);
        let result = tool
            .call(CompileTypstArgs {
                content: "= Test\n\nHello, world! $ E = m c^2 $".to_string(),
                output_path,
            })
            .await;

        assert!(result.is_ok(), "compile failed: {:?}", result.err());
        let output = result.unwrap();
        assert_eq!(output.page_count, 1);
        assert!(std::path::Path::new(&output.saved_path).exists());
    }

    #[tokio::test]
    async fn test_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let output_path = dir
            .path()
            .join("nested/dir/output.pdf")
            .to_string_lossy()
            .into_owned();

        let tool = CompileTypstTool::new(None);
        let result = tool
            .call(CompileTypstArgs {
                content: "= Nested\n\nContent here.".to_string(),
                output_path,
            })
            .await;

        assert!(result.is_ok());
        assert!(std::path::Path::new(&result.unwrap().saved_path).exists());
    }

    #[tokio::test]
    async fn test_invalid_typst_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let output_path = dir.path().join("fail.pdf").to_string_lossy().into_owned();

        let tool = CompileTypstTool::new(None);
        let result = tool
            .call(CompileTypstArgs {
                content: "#nonexistent-function-xyz()".to_string(),
                output_path,
            })
            .await;

        assert!(result.is_err());
    }
}
