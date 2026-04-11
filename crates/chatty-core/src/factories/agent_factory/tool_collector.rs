use rig::tool::ToolDyn;

use crate::tools::{
    AddAttachmentTool, BrowserUseTool, CompileTypstTool, CreateChartTool,
    DaytonaTool, DescribeDataTool, EditExcelTool, ExecuteCodeTool,
    FetchTool, FindDefinitionTool, FindFilesTool, GlobSearchTool, InvokeAgentTool,
    ListAgentsTool, ListDirectoryTool, ListToolsTool, MoveFileTool,
    PdfExtractTextTool, PdfInfoTool, PdfToImageTool, PublishModuleTool, QueryDataTool,
    ReadBinaryTool, ReadExcelTool, ReadFileTool, ReadSkillTool, RememberTool, SaveSkillTool,
    SearchCodeTool, SearchMemoryTool, SearchWebTool, ShellCdTool, ShellExecuteTool,
    ShellSetEnvTool, ShellStatusTool, SubAgentTool, WriteExcelTool, WriteFileTool,
    CreateDirectoryTool, DeleteFileTool, ApplyDiffTool,
    GitStatusTool, GitDiffTool, GitLogTool, GitAddTool, GitCreateBranchTool,
    GitSwitchBranchTool, GitCommitTool,
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
#[allow(clippy::too_many_arguments)]
pub(super) fn collect_tools(
    list_tools: ListToolsTool,
    fs_read: Option<FsReadTools>,
    fs_write: Option<FsWriteTools>,
    add_attachment: Option<AddAttachmentTool>,
    pdf_to_image: Option<PdfToImageTool>,
    pdf_info: Option<PdfInfoTool>,
    pdf_extract_text: Option<PdfExtractTextTool>,
    mcp_mgmt: McpTools,
    fetch_tool: Option<FetchTool>,
    shell_tools: Option<ShellTools>,
    git_tools: Option<GitTools>,
    search_tools: Option<SearchTools>,
    excel_read: Option<ReadExcelTool>,
    excel_write: Option<ExcelWriteTools>,
    data_query: Option<DataQueryTools>,
    chart_tool: Option<CreateChartTool>,
    typst_tool: Option<CompileTypstTool>,
    execute_code_tool: Option<ExecuteCodeTool>,
    remember_tool: Option<RememberTool>,
    save_skill_tool: Option<SaveSkillTool>,
    search_memory_tool: Option<SearchMemoryTool>,
    read_skill_tool: ReadSkillTool,
    search_web_tool: Option<SearchWebTool>,
    sub_agent_tool: Option<SubAgentTool>,
    browser_use_tool: Option<BrowserUseTool>,
    daytona_tool: Option<DaytonaTool>,
    list_agents_tool: ListAgentsTool,
    invoke_agent_tool: InvokeAgentTool,
    publish_module_tool: Option<PublishModuleTool>,
) -> Vec<Box<dyn ToolDyn>> {
    let mut tools: Vec<Box<dyn ToolDyn>> = Vec::new();
    tools.push(Box::new(list_tools)); // always present
    tools.push(Box::new(list_agents_tool)); // always present
    tools.push(Box::new(invoke_agent_tool)); // always present
    if let Some(t) = mcp_mgmt.list {
        tools.push(Box::new(t));
    }
    if let Some(t) = mcp_mgmt.add {
        tools.push(Box::new(t));
    }
    if let Some(t) = mcp_mgmt.delete {
        tools.push(Box::new(t));
    }
    if let Some(t) = mcp_mgmt.edit {
        tools.push(Box::new(t));
    }
    if let Some((rf, rb, ld, gs)) = fs_read {
        tools.push(Box::new(rf));
        tools.push(Box::new(rb));
        tools.push(Box::new(ld));
        tools.push(Box::new(gs));
    }
    if let Some((wf, cd, df, mf, ad)) = fs_write {
        tools.push(Box::new(wf));
        tools.push(Box::new(cd));
        tools.push(Box::new(df));
        tools.push(Box::new(mf));
        tools.push(Box::new(ad));
    }
    if let Some(t) = add_attachment {
        tools.push(Box::new(t));
    }
    if let Some(t) = pdf_to_image {
        tools.push(Box::new(t));
    }
    if let Some(t) = pdf_info {
        tools.push(Box::new(t));
    }
    if let Some(t) = pdf_extract_text {
        tools.push(Box::new(t));
    }
    if let Some(t) = fetch_tool {
        tools.push(Box::new(t));
    }
    if let Some((exec, set_env, cd, status)) = shell_tools {
        tools.push(Box::new(exec));
        tools.push(Box::new(set_env));
        tools.push(Box::new(cd));
        tools.push(Box::new(status));
    }
    if let Some((status, diff, log, add, create_branch, switch_branch, commit)) = git_tools {
        tools.push(Box::new(status));
        tools.push(Box::new(diff));
        tools.push(Box::new(log));
        tools.push(Box::new(add));
        tools.push(Box::new(create_branch));
        tools.push(Box::new(switch_branch));
        tools.push(Box::new(commit));
    }
    if let Some((sc, ff, fd)) = search_tools {
        tools.push(Box::new(sc));
        tools.push(Box::new(ff));
        tools.push(Box::new(fd));
    }
    if let Some(t) = excel_read {
        tools.push(Box::new(t));
    }
    if let Some((wt, et)) = excel_write {
        tools.push(Box::new(wt));
        tools.push(Box::new(et));
    }
    if let Some((qt, dt)) = data_query {
        tools.push(Box::new(qt));
        tools.push(Box::new(dt));
    }
    if let Some(t) = chart_tool {
        tools.push(Box::new(t));
    }
    if let Some(t) = typst_tool {
        tools.push(Box::new(t));
    }
    if let Some(t) = execute_code_tool {
        tools.push(Box::new(t));
    }
    if let Some(t) = remember_tool {
        tools.push(Box::new(t));
    }
    if let Some(t) = save_skill_tool {
        tools.push(Box::new(t));
    }
    if let Some(t) = search_memory_tool {
        tools.push(Box::new(t));
    }
    tools.push(Box::new(read_skill_tool));
    if let Some(t) = search_web_tool {
        tools.push(Box::new(t));
    }
    if let Some(t) = sub_agent_tool {
        tools.push(Box::new(t));
    }
    if let Some(t) = browser_use_tool {
        tools.push(Box::new(t));
    }
    if let Some(t) = daytona_tool {
        tools.push(Box::new(t));
    }
    if let Some(t) = publish_module_tool {
        tools.push(Box::new(t));
    }
    tools
}
