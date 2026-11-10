use super::path_utils::{approve_output_write, resolve_workspace_output, write_output_file};
use crate::models::write_approval_store::PendingWriteApprovals;
use crate::services::filesystem_service::FileSystemService;
use crate::services::typst_compiler_service::TypstCompilerService;
use crate::settings::models::execution_settings::ApprovalMode;
use crate::tools::ToolError;
#[cfg(test)]
use rig_agent::tool::tool_definition;
use rig_agent::tool::{Tool, ToolContext, ToolExecutionError};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Arguments for the compile_typst tool.
#[derive(Deserialize, Serialize)]
pub struct CompileTypstArgs {
    /// Typst markup source to compile. Supports headings, paragraphs, tables,
    /// math expressions, code blocks, lists, and all other Typst features.
    pub content: String,
    /// File path where the PDF will be saved, inside the workspace: relative
    /// to the workspace root, or absolute within it.
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

/// Tool that compiles Typst markup to a PDF file in the workspace.
#[derive(Clone)]
pub struct CompileTypstTool {
    service: Arc<FileSystemService>,
    write_approval: Option<(ApprovalMode, PendingWriteApprovals)>,
}

impl CompileTypstTool {
    pub fn new(service: Arc<FileSystemService>) -> Self {
        Self {
            service,
            write_approval: None,
        }
    }

    /// Ask for write approval under `mode` before every write (AGE-534,
    /// as AGE-1096 did for `write_docx`). The agent factory always sets
    /// this; `new` alone is for callers that are not an agent (tests).
    pub fn with_write_approval(
        mut self,
        mode: ApprovalMode,
        pending: PendingWriteApprovals,
    ) -> Self {
        self.write_approval = Some((mode, pending));
        self
    }
}

impl Tool for CompileTypstTool {
    const NAME: &'static str = "compile_typst";
    type Error = ToolError;
    type Args = CompileTypstArgs;
    type Output = CompileTypstOutput;

    fn description(&self) -> String {
        "Compile Typst markup into a PDF file and save it to disk.\n\
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
                The output_path is relative to the workspace root (or absolute within it).\n\
                Parent directories are created automatically.\n\
                \n\
                Example:\n\
                content: \"= Sales Report\\n\\n#set text(size: 11pt)\\n\\nTotal revenue: $ 1.2 times 10^6 $\\n\\n\
                #table(columns: 2, [Month], [Revenue], [Jan], [$100K], [Feb], [$120K])\"\n\
                output_path: \"reports/sales_q1.pdf\""
                .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "content": {
                    "type": "string",
                    "description": "Typst markup source to compile into a PDF"
                },
                "output_path": {
                    "type": "string",
                    "description": "File path where the PDF will be saved, relative to the workspace root"
                }
            },
            "required": ["content", "output_path"]
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
        // Inside the workspace only, symlinks resolved (AGE-534).
        let root = self.service.workspace_root();
        let resolved = resolve_workspace_output(root, &args.output_path)
            .map_err(ToolError::OperationFailed)?;

        // Compile first: a markup error needs no approval prompt.
        let (pdf_bytes, page_count) =
            TypstCompilerService::compile_to_pdf(&args.content, Some(root))
                .map_err(|e| ToolError::OperationFailed(e.to_string()))?;

        let approved = approve_output_write(
            self.write_approval.as_ref(),
            &resolved,
            format!("PDF, {page_count} page(s), {} bytes", pdf_bytes.len()),
        )
        .await
        .map_err(ToolError::OperationFailed)?;
        if !approved {
            return Err(ToolError::OperationFailed(
                "Write denied by user".to_string(),
            ));
        }

        write_output_file(root, &resolved, &pdf_bytes).map_err(ToolError::OperationFailed)?;

        Ok(CompileTypstOutput {
            saved_path: resolved.to_string_lossy().into_owned(),
            page_count,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig_agent::tool::{Tool, ToolContext};

    async fn service(dir: &tempfile::TempDir) -> Arc<FileSystemService> {
        Arc::new(
            FileSystemService::new(dir.path().to_str().unwrap())
                .await
                .unwrap(),
        )
    }

    fn args(content: &str, output_path: &str) -> CompileTypstArgs {
        CompileTypstArgs {
            content: content.to_string(),
            output_path: output_path.to_string(),
        }
    }

    #[tokio::test]
    async fn test_definition_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let tool = CompileTypstTool::new(service(&dir).await);
        let def = tool_definition(&tool);
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

        let tool = CompileTypstTool::new(service(&dir).await);
        let result = tool
            .call(
                &mut ToolContext::new(),
                args("= Test\n\nHello, world! $ E = m c^2 $", &output_path),
            )
            .await;

        assert!(result.is_ok(), "compile failed: {:?}", result.err());
        let output = result.unwrap();
        assert_eq!(output.page_count, 1);
        assert!(std::path::Path::new(&output.saved_path).exists());
    }

    #[tokio::test]
    async fn test_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let tool = CompileTypstTool::new(service(&dir).await);
        let result = tool
            .call(
                &mut ToolContext::new(),
                args("= Nested\n\nContent here.", "nested/dir/output.pdf"),
            )
            .await;

        assert!(result.is_ok());
        assert!(std::path::Path::new(&result.unwrap().saved_path).exists());
    }

    #[tokio::test]
    async fn test_invalid_typst_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let tool = CompileTypstTool::new(service(&dir).await);
        let result = tool
            .call(
                &mut ToolContext::new(),
                args("#nonexistent-function-xyz()", "fail.pdf"),
            )
            .await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn compile_typst_refuses_paths_outside_the_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let tool = CompileTypstTool::new(service(&dir).await);
        let escape = outside.path().join("x.pdf");
        for path in ["~", "~/../../x.pdf", "../x.pdf", escape.to_str().unwrap()] {
            let err = tool
                .call(&mut ToolContext::new(), args("= Hi", path))
                .await
                .unwrap_err();
            assert!(
                err.to_string().contains("not allowed")
                    || err.to_string().contains("outside the workspace"),
                "{path}: {err}"
            );
        }
        assert!(!escape.exists());
    }

    #[tokio::test]
    async fn compile_typst_asks_before_writing_under_always_ask() {
        use std::time::Duration;

        use crate::models::write_approval_store::{WriteApprovalDecision, WriteApprovalStore};

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("report.pdf");

        let mut store = WriteApprovalStore::new();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let (resolution_tx, _resolution_rx) = tokio::sync::mpsc::unbounded_channel();
        store.set_notifiers(tx, resolution_tx);
        let tool = CompileTypstTool::new(service(&dir).await)
            .with_write_approval(ApprovalMode::AlwaysAsk, store.get_pending_approvals());

        let spawn = |tool: CompileTypstTool| {
            tokio::spawn(async move {
                tool.call(&mut ToolContext::new(), args("= Report", "report.pdf"))
                    .await
                    .map(|_| ())
            })
        };

        // Denied: the call fails and nothing is written.
        let denied = spawn(tool.clone());
        let asked = tokio::time::timeout(Duration::from_secs(30), rx.recv())
            .await
            .expect("the write is asked about")
            .expect("notifier open");
        assert!(store.resolve(&asked.id, WriteApprovalDecision::Denied));
        assert!(denied.await.unwrap().is_err(), "a denied write fails");
        assert!(!target.exists(), "a denied write writes nothing");

        // Approved: the call succeeds and the file exists.
        let approved = spawn(tool.clone());
        let asked = tokio::time::timeout(Duration::from_secs(30), rx.recv())
            .await
            .expect("the second write is asked about")
            .expect("notifier open");
        assert!(store.resolve(&asked.id, WriteApprovalDecision::Approved));
        approved.await.unwrap().unwrap();
        assert!(target.exists(), "an approved write is written");

        // Overwrite: the card says so.
        let again = spawn(tool);
        let asked = tokio::time::timeout(Duration::from_secs(30), rx.recv())
            .await
            .expect("the overwrite is asked about")
            .expect("notifier open");
        assert!(
            asked.command.starts_with("Overwrite file:"),
            "{}",
            asked.command
        );
        assert!(store.resolve(&asked.id, WriteApprovalDecision::Denied));
        assert!(again.await.unwrap().is_err());
    }
}
