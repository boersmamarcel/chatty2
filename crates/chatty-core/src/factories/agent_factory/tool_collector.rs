use rig::tool::ToolDyn;

use crate::tools::{
    AddAttachmentTool, ApplyDiffTool, BrowserUseTool, CompileTypstTool, CreateChartTool,
    CreateDirectoryTool, DaytonaTool, DeleteFileTool, DescribeDataTool, EditExcelTool,
    ExecuteCodeTool, FetchTool, FindDefinitionTool, FindFilesTool, GitAddTool, GitCommitTool,
    GitCreateBranchTool, GitDiffTool, GitLogTool, GitStatusTool, GitSwitchBranchTool,
    GlobSearchTool, InvokeAgentTool, ListAgentsTool, ListDirectoryTool, ListToolsTool,
    MoveFileTool, PdfExtractTextTool, PdfInfoTool, PdfToImageTool, PublishModuleTool,
    QueryDataTool, ReadBinaryTool, ReadExcelTool, ReadFileTool, ReadSkillTool, RememberTool,
    SaveSkillTool, SearchCodeTool, SearchMemoryTool, SearchWebTool, ShellCdTool, ShellExecuteTool,
    ShellSetEnvTool, ShellStatusTool, SubAgentTool, WriteExcelTool, WriteFileTool,
};

use super::mcp_helpers::McpTools;

/// Filesystem read tool set
pub(super) type FsReadTools = (
    ReadFileTool,
    ReadBinaryTool,
    ListDirectoryTool,
    GlobSearchTool,
);

/// Filesystem write tool set
pub(super) type FsWriteTools = (
    WriteFileTool,
    CreateDirectoryTool,
    DeleteFileTool,
    MoveFileTool,
    ApplyDiffTool,
);

/// Shell session tool set (all four shell tools)
pub(super) type ShellTools = (
    ShellExecuteTool,
    ShellSetEnvTool,
    ShellCdTool,
    ShellStatusTool,
);

/// Git integration tool set (seven git tools)
pub(super) type GitTools = (
    GitStatusTool,
    GitDiffTool,
    GitLogTool,
    GitAddTool,
    GitCreateBranchTool,
    GitSwitchBranchTool,
    GitCommitTool,
);

/// Code search tool set (search_code, find_files, find_definition)
pub(super) type SearchTools = (SearchCodeTool, FindFilesTool, FindDefinitionTool);

/// Excel tool sets (gated on filesystem read/write settings)
pub(super) type ExcelWriteTools = (WriteExcelTool, EditExcelTool);

/// DuckDB data query tools (gated on filesystem_read_enabled)
pub(super) type DataQueryTools = (QueryDataTool, DescribeDataTool);

/// Collect all optional native tools into a `Vec<Box<dyn ToolDyn>>`.
///
/// Replaces the former 16-branch `build_agent_with_tools!` macro. Adding a new
/// optional tool only requires one new `if let Some` block here — no combinatorial
/// branching.
pub(super) struct NativeTools {
    pub list_tools: ListToolsTool,
    pub fs_read: Option<FsReadTools>,
    pub fs_write: Option<FsWriteTools>,
    pub add_attachment: Option<AddAttachmentTool>,
    pub pdf_to_image: Option<PdfToImageTool>,
    pub pdf_info: Option<PdfInfoTool>,
    pub pdf_extract_text: Option<PdfExtractTextTool>,
    pub mcp_mgmt: McpTools,
    pub fetch_tool: Option<FetchTool>,
    pub shell_tools: Option<ShellTools>,
    pub git_tools: Option<GitTools>,
    pub search_tools: Option<SearchTools>,
    pub excel_read: Option<ReadExcelTool>,
    pub excel_write: Option<ExcelWriteTools>,
    pub data_query: Option<DataQueryTools>,
    pub chart_tool: Option<CreateChartTool>,
    pub typst_tool: Option<CompileTypstTool>,
    pub execute_code_tool: Option<ExecuteCodeTool>,
    pub remember_tool: Option<RememberTool>,
    pub save_skill_tool: Option<SaveSkillTool>,
    pub search_memory_tool: Option<SearchMemoryTool>,
    pub read_skill_tool: ReadSkillTool,
    pub search_web_tool: Option<SearchWebTool>,
    pub sub_agent_tool: Option<SubAgentTool>,
    pub browser_use_tool: Option<BrowserUseTool>,
    pub daytona_tool: Option<DaytonaTool>,
    pub list_agents_tool: ListAgentsTool,
    pub invoke_agent_tool: InvokeAgentTool,
    pub publish_module_tool: Option<PublishModuleTool>,
}

impl NativeTools {
    /// Consume self and produce a flat `Vec<Box<dyn ToolDyn>>`.
    pub fn into_tool_vec(self) -> Vec<Box<dyn ToolDyn>> {
        let mut tools: Vec<Box<dyn ToolDyn>> = Vec::new();
        tools.push(Box::new(self.list_tools)); // always present
        tools.push(Box::new(self.list_agents_tool)); // always present
        tools.push(Box::new(self.invoke_agent_tool)); // always present
        if let Some(t) = self.mcp_mgmt.list {
            tools.push(Box::new(t));
        }
        if let Some(t) = self.mcp_mgmt.add {
            tools.push(Box::new(t));
        }
        if let Some(t) = self.mcp_mgmt.delete {
            tools.push(Box::new(t));
        }
        if let Some(t) = self.mcp_mgmt.edit {
            tools.push(Box::new(t));
        }
        if let Some((rf, rb, ld, gs)) = self.fs_read {
            tools.push(Box::new(rf));
            tools.push(Box::new(rb));
            tools.push(Box::new(ld));
            tools.push(Box::new(gs));
        }
        if let Some((wf, cd, df, mf, ad)) = self.fs_write {
            tools.push(Box::new(wf));
            tools.push(Box::new(cd));
            tools.push(Box::new(df));
            tools.push(Box::new(mf));
            tools.push(Box::new(ad));
        }
        if let Some(t) = self.add_attachment {
            tools.push(Box::new(t));
        }
        if let Some(t) = self.pdf_to_image {
            tools.push(Box::new(t));
        }
        if let Some(t) = self.pdf_info {
            tools.push(Box::new(t));
        }
        if let Some(t) = self.pdf_extract_text {
            tools.push(Box::new(t));
        }
        if let Some(t) = self.fetch_tool {
            tools.push(Box::new(t));
        }
        if let Some((exec, set_env, cd, status)) = self.shell_tools {
            tools.push(Box::new(exec));
            tools.push(Box::new(set_env));
            tools.push(Box::new(cd));
            tools.push(Box::new(status));
        }
        if let Some((status, diff, log, add, create_branch, switch_branch, commit)) = self.git_tools
        {
            tools.push(Box::new(status));
            tools.push(Box::new(diff));
            tools.push(Box::new(log));
            tools.push(Box::new(add));
            tools.push(Box::new(create_branch));
            tools.push(Box::new(switch_branch));
            tools.push(Box::new(commit));
        }
        if let Some((sc, ff, fd)) = self.search_tools {
            tools.push(Box::new(sc));
            tools.push(Box::new(ff));
            tools.push(Box::new(fd));
        }
        if let Some(t) = self.excel_read {
            tools.push(Box::new(t));
        }
        if let Some((wt, et)) = self.excel_write {
            tools.push(Box::new(wt));
            tools.push(Box::new(et));
        }
        if let Some((qt, dt)) = self.data_query {
            tools.push(Box::new(qt));
            tools.push(Box::new(dt));
        }
        if let Some(t) = self.chart_tool {
            tools.push(Box::new(t));
        }
        if let Some(t) = self.typst_tool {
            tools.push(Box::new(t));
        }
        if let Some(t) = self.execute_code_tool {
            tools.push(Box::new(t));
        }
        if let Some(t) = self.remember_tool {
            tools.push(Box::new(t));
        }
        if let Some(t) = self.save_skill_tool {
            tools.push(Box::new(t));
        }
        if let Some(t) = self.search_memory_tool {
            tools.push(Box::new(t));
        }
        tools.push(Box::new(self.read_skill_tool));
        if let Some(t) = self.search_web_tool {
            tools.push(Box::new(t));
        }
        if let Some(t) = self.sub_agent_tool {
            tools.push(Box::new(t));
        }
        if let Some(t) = self.browser_use_tool {
            tools.push(Box::new(t));
        }
        if let Some(t) = self.daytona_tool {
            tools.push(Box::new(t));
        }
        if let Some(t) = self.publish_module_tool {
            tools.push(Box::new(t));
        }
        tools
    }
}
