/// Shared error type for tools with simple failure modes.
/// Tools with genuinely distinct error categories keep their own types.
#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("{0}")]
    OperationFailed(String),
}

impl From<anyhow::Error> for ToolError {
    fn from(e: anyhow::Error) -> Self {
        ToolError::OperationFailed(e.to_string())
    }
}

/// Normalize a tool's typed error into rig's envelope **without redacting the
/// message**.
///
/// rig's default `Tool::map_error` is `ToolExecutionError::from_error`, which
/// treats an arbitrary source error as operator-only: it keeps the real message
/// for diagnostics and hands the model the stable kind feedback instead. For
/// `ToolErrorKind::Other` that feedback is the literal string
/// `"the tool failed"` (rig-core `src/tool/result.rs`), so a browser navigation
/// that died on `ERR_NETWORK_CHANGED` reached both the model and the transcript
/// as four content-free words (AGE-187).
///
/// Building the error explicitly keeps the message model-visible, which is what
/// rig prescribes for deliberately authored failures. The trade-off is
/// deliberate: our tool errors are strings we write ourselves, and a failure the
/// user cannot read is a dead end when debugging. Do not route provider or
/// third-party error text through here without checking it first — that is the
/// case rig's redaction exists for.
pub fn map_tool_error<E>(tool_name: &str, error: E) -> rig_agent::tool::ToolExecutionError
where
    E: std::error::Error + Send + Sync + 'static,
{
    let message = error.to_string();
    let kind = classify_tool_error(&message);
    rig_agent::tool::ToolExecutionError::new(kind, format!("{tool_name}: {message}"))
        .with_source(error)
}

/// Best-effort classification of a tool failure from its message.
///
/// Only affects rig's retryability hint and telemetry — the message reaches the
/// model either way. Kept deliberately small: a wrong guess here is cosmetic,
/// and an over-fitted matcher would be worse than `Other`.
fn classify_tool_error(message: &str) -> rig_agent::tool::ToolErrorKind {
    use rig_agent::tool::ToolErrorKind;

    let m = message.to_ascii_lowercase();
    if m.contains("timed out") || m.contains("timeout") {
        ToolErrorKind::Timeout
    } else if m.contains("permission denied")
        || m.contains("not allowed")
        || m.contains("outside the workspace")
        || m.contains("denied by user")
    {
        ToolErrorKind::PermissionDenied
    } else if m.contains("no such file") || m.contains("not found") {
        ToolErrorKind::NotFound
    } else if m.contains("err_")
        || m.contains("connection")
        || m.contains("network")
        || m.contains("dns")
        || m.contains("unreachable")
    {
        ToolErrorKind::Network
    } else {
        ToolErrorKind::Other
    }
}

#[cfg(test)]
mod map_tool_error_tests {
    use super::*;

    /// The regression this exists for: rig redacted every typed tool error to
    /// "the tool failed" before it reached the model or the transcript.
    #[test]
    fn message_survives_into_model_feedback() {
        let err = ToolError::OperationFailed(
            "navigation failed: ERR_NETWORK_CHANGED (A network change was detected)".into(),
        );
        let mapped = map_tool_error("browser_navigate", err);

        let feedback = mapped.model_feedback().unwrap_or_default();
        assert!(
            feedback.contains("ERR_NETWORK_CHANGED"),
            "the model must see the real error, got {feedback:?}"
        );
        assert!(
            feedback.contains("browser_navigate"),
            "the model must see which tool failed, got {feedback:?}"
        );
        assert_ne!(feedback, "the tool failed");
    }

    #[test]
    fn transport_failures_classify_as_network() {
        let mapped = map_tool_error(
            "browser_navigate",
            ToolError::OperationFailed("ERR_NETWORK_CHANGED".into()),
        );
        assert_eq!(mapped.kind(), rig_agent::tool::ToolErrorKind::Network);
    }

    #[test]
    fn workspace_refusals_classify_as_permission_denied() {
        let mapped = map_tool_error(
            "write_file",
            ToolError::OperationFailed("path is outside the workspace".into()),
        );
        assert_eq!(
            mapped.kind(),
            rig_agent::tool::ToolErrorKind::PermissionDenied
        );
    }

    #[test]
    fn unclassifiable_failures_still_carry_their_message() {
        let mapped = map_tool_error(
            "some_tool",
            ToolError::OperationFailed("something specific went wrong".into()),
        );
        assert_eq!(mapped.kind(), rig_agent::tool::ToolErrorKind::Other);
        assert!(
            mapped
                .model_feedback()
                .unwrap_or_default()
                .contains("something specific went wrong")
        );
    }
}

pub mod add_attachment_tool;
pub mod agent_todo_tool;
#[cfg(feature = "browser")]
pub mod browser_tools;
pub mod browser_use_tool;
pub mod chart_tool;
#[cfg(feature = "duckdb")]
pub mod data_query_tool;
pub mod daytona_tool;
pub mod doc_retriever_tool;
#[cfg(feature = "docx")]
pub mod docx_tool;
#[cfg(feature = "excel")]
pub mod excel_tool;
pub mod execute_code_tool;
pub mod fetch_tool;
#[cfg(feature = "duckdb")]
pub mod file_structure_tool;
pub mod filesystem_tool;
pub mod filesystem_write_tool;
pub mod git_tool;
pub mod invoke_agent_tool;
pub mod list_agents_tool;
pub mod list_mcp_tool;
pub mod list_tools_tool;
mod path_utils;
#[cfg(feature = "pdf")]
pub mod pdf_extract_text_tool;
#[cfg(feature = "pdf")]
pub mod pdf_info_tool;
#[cfg(feature = "pdf")]
pub mod pdf_to_image_tool;
#[cfg(feature = "pptx")]
pub mod pptx_tool;
pub mod publish_module_tool;
pub mod read_skill_tool;
pub mod remember_tool;
pub mod save_skill_tool;
pub mod search_memory_tool;
pub mod search_tool;
pub mod search_web_tool;
pub mod shell_tool;
pub mod sub_agent_tool;
#[cfg(test)]
pub mod test_helpers;
#[cfg(feature = "math-render")]
pub mod typst_tool;

pub use add_attachment_tool::{AddAttachmentTool, PendingArtifacts};
pub use agent_todo_tool::{UpdateTodoTool, VerifyCompletionTool, WriteTodosTool};
pub use browser_use_tool::BrowserUseTool;
pub use chart_tool::CreateChartTool;
#[cfg(feature = "duckdb")]
pub use data_query_tool::{DescribeDataTool, ProfileDataTool, QueryDataTool};
pub use daytona_tool::DaytonaTool;
pub use doc_retriever_tool::DocRetrieverTool;
#[cfg(feature = "docx")]
pub use docx_tool::{ReadDocxTool, WriteDocxTool};
#[cfg(feature = "excel")]
pub use excel_tool::{EditExcelTool, ReadExcelTool, WriteExcelTool};
pub use execute_code_tool::ExecuteCodeTool;
pub use fetch_tool::FetchTool;
#[cfg(feature = "duckdb")]
pub use file_structure_tool::FileStructureTool;
pub use filesystem_tool::{GlobSearchTool, ListDirectoryTool, ReadBinaryTool, ReadFileTool};
pub use filesystem_write_tool::{
    ApplyDiffTool, CreateDirectoryTool, DeleteFileTool, FinalAnswerTool, MoveFileTool,
    WriteFileTool,
};
pub use git_tool::{
    GitAddTool, GitCommitTool, GitCreateBranchTool, GitDiffTool, GitLogTool, GitStatusTool,
    GitSwitchBranchTool,
};
pub use invoke_agent_tool::InvokeAgentTool;
pub use list_agents_tool::{ListAgentsTool, LocalModuleAgentSummary};
pub use list_mcp_tool::ListMcpTool;
pub use list_tools_tool::ListToolsTool;
#[cfg(feature = "pdf")]
pub use pdf_extract_text_tool::PdfExtractTextTool;
#[cfg(feature = "pdf")]
pub use pdf_info_tool::PdfInfoTool;
#[cfg(feature = "pdf")]
pub use pdf_to_image_tool::PdfToImageTool;
#[cfg(feature = "pptx")]
pub use pptx_tool::{ReadPptxTool, WritePptxTool};
pub use publish_module_tool::PublishModuleTool;
pub use read_skill_tool::ReadSkillTool;
pub use remember_tool::RememberTool;
pub use save_skill_tool::{SKILL_TITLE_PREFIX, SaveSkillTool};
pub use search_memory_tool::{
    SearchMemoryTool, build_memory_context_block, merge_search_results, select_context_hits,
};
pub use search_tool::{FindDefinitionTool, FindFilesTool, SearchCodeTool};
pub use search_web_tool::SearchWebTool;
pub use shell_tool::{ShellCdTool, ShellExecuteTool, ShellSetEnvTool, ShellStatusTool};
pub use sub_agent_tool::{CHATTY_PROGRESS_PREFIX, SubAgentTool, is_chatty_progress_line};
#[cfg(feature = "math-render")]
pub use typst_tool::CompileTypstTool;

/// Guard tests: every built-in tool's parameter schema must convert to a valid
/// Gemini `Schema` without any empty `type` strings.
///
/// Gemini rejects requests containing `type: ""` (produced by rig's `infer_type`
/// when a schema object has no `type`, no `properties`, and no composition
/// keywords).  These tests catch regressions if the rig-core vendor patch is
/// ever removed or if a new tool introduces a schema gap.
#[cfg(test)]
mod gemini_compat_tests {
    use rig_core::completion::ToolDefinition;
    use rig_core::providers::gemini::completion::gemini_api_types::{Schema, Tool};

    /// Recursively assert that every [`Schema`] node has a non-empty `type`.
    fn assert_no_empty_types(schema: &Schema, path: &str) {
        assert!(
            !schema.r#type.is_empty(),
            "Gemini schema 'type' is empty at path '{path}'"
        );
        if let Some(items) = &schema.items {
            assert_no_empty_types(items, &format!("{path}.items"));
        }
        if let Some(props) = &schema.properties {
            for (key, val) in props {
                assert_no_empty_types(val, &format!("{path}.{key}"));
            }
        }
    }

    /// Convert a [`ToolDefinition`] to a Gemini `Tool` and assert all types
    /// are non-empty.
    fn check_gemini_compat(def: ToolDefinition) {
        let name = def.name.clone();
        let tool = Tool::try_from(def)
            .unwrap_or_else(|e| panic!("Tool '{name}' failed Gemini conversion: {e}"));
        for decl in &tool.function_declarations {
            if let Some(params) = &decl.parameters {
                assert_no_empty_types(params, &name);
            }
        }
    }

    #[tokio::test]
    async fn fetch_tool_gemini_compat() {
        use crate::tools::fetch_tool::FetchTool;
        let tool = FetchTool::new(None);
        check_gemini_compat(rig_agent::tool::tool_definition(&tool));
    }

    #[tokio::test]
    async fn chart_tool_gemini_compat() {
        use crate::tools::chart_tool::CreateChartTool;
        let tool = CreateChartTool::new(None, None);
        check_gemini_compat(rig_agent::tool::tool_definition(&tool));
    }

    #[tokio::test]
    async fn daytona_tool_gemini_compat() {
        use crate::tools::daytona_tool::DaytonaTool;
        let tool = DaytonaTool::new("dummy".to_string(), None);
        check_gemini_compat(rig_agent::tool::tool_definition(&tool));
    }

    #[tokio::test]
    async fn search_web_tool_gemini_compat() {
        use crate::tools::search_web_tool::SearchWebTool;
        let tool = SearchWebTool::new_fallback(10);
        check_gemini_compat(rig_agent::tool::tool_definition(&tool));
    }
}
