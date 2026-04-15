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

pub mod add_attachment_tool;
pub mod browser_use_tool;
pub mod chart_tool;
#[cfg(feature = "duckdb")]
pub mod data_query_tool;
pub mod daytona_tool;
#[cfg(feature = "excel")]
pub mod excel_tool;
pub mod execute_code_tool;
pub mod fetch_tool;
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
pub use browser_use_tool::BrowserUseTool;
pub use chart_tool::CreateChartTool;
#[cfg(feature = "duckdb")]
pub use data_query_tool::{DescribeDataTool, QueryDataTool};
pub use daytona_tool::DaytonaTool;
#[cfg(feature = "excel")]
pub use excel_tool::{EditExcelTool, ReadExcelTool, WriteExcelTool};
pub use execute_code_tool::ExecuteCodeTool;
pub use fetch_tool::FetchTool;
pub use filesystem_tool::{GlobSearchTool, ListDirectoryTool, ReadBinaryTool, ReadFileTool};
pub use filesystem_write_tool::{
    ApplyDiffTool, CreateDirectoryTool, DeleteFileTool, MoveFileTool, WriteFileTool,
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
pub use sub_agent_tool::SubAgentTool;
#[cfg(feature = "math-render")]
pub use typst_tool::CompileTypstTool;
