use rig_agent::agent::{AgentBuilder, WithBuilderTools};
use rig_agent::tool::Tool;

#[cfg(feature = "math-render")]
use crate::tools::CompileTypstTool;
#[cfg(feature = "browser")]
use crate::tools::browser_tools::BrowserTools;
use crate::tools::{
    AddAttachmentTool, ApplyDiffTool, AskUserTool, BrowserUseTool, CreateChartTool,
    CreateDirectoryTool, DaytonaTool, DeleteFileTool, DocRetrieverTool, ExecuteCodeTool, FetchTool,
    FinalAnswerTool, FindDefinitionTool, FindFilesTool, GitAddTool, GitCommitTool,
    GitCreateBranchTool, GitDiffTool, GitLogTool, GitStatusTool, GitSwitchBranchTool,
    GlobSearchTool, InvokeAgentTool, ListAgentsTool, ListDirectoryTool, ListToolsTool,
    MoveFileTool, PublishModuleTool, ReadBinaryTool, ReadFileTool, ReadSkillTool, RememberTool,
    SaveSkillTool, SearchCodeTool, SearchMemoryTool, SearchWebTool, ShellCdTool, ShellExecuteTool,
    ShellSetEnvTool, ShellStatusTool, UpdateTodoTool, VerifyCompletionTool, WriteFileTool,
    WriteTodosTool,
};
#[cfg(feature = "duckdb")]
use crate::tools::{DescribeDataTool, FileStructureTool, ProfileDataTool, QueryDataTool};
#[cfg(feature = "excel")]
use crate::tools::{EditExcelTool, ReadExcelTool, WriteExcelTool};
#[cfg(feature = "pdf")]
use crate::tools::{PdfExtractTextTool, PdfInfoTool, PdfToImageTool};
#[cfg(feature = "docx")]
use crate::tools::{ReadDocxTool, WriteDocxTool};
#[cfg(feature = "pptx")]
use crate::tools::{ReadPptxTool, WritePptxTool};

use super::mcp_helpers::McpTools;
use super::tool_profile::ToolProfile;

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
    FinalAnswerTool,
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
#[cfg(feature = "excel")]
pub(super) type ExcelWriteTools = (WriteExcelTool, EditExcelTool);

/// DuckDB data query tools (gated on filesystem_read_enabled)
#[cfg(feature = "duckdb")]
pub(super) type DataQueryTools = (
    QueryDataTool,
    DescribeDataTool,
    ProfileDataTool,
    FileStructureTool,
);

/// Collect all optional native tools and register them on an [`AgentBuilder`].
///
/// In rig 0.42, typed tools are registered via repeated `.tool(T)` calls —
/// there is no public `ToolDyn` erasure. Adding a new optional tool only
/// requires one new `if let Some` block here.
pub(super) struct NativeTools {
    /// The role's tool allowlist, or `None` for every tool collected here
    /// (ADR-0011 C11). Filtering happens at registration rather than at
    /// construction: a tool is cheap to build and its schema is what costs
    /// the worker its context.
    pub tool_profile: Option<&'static ToolProfile>,
    pub list_tools: ListToolsTool,
    pub write_todos_tool: WriteTodosTool,
    pub update_todo_tool: UpdateTodoTool,
    pub verify_completion_tool: VerifyCompletionTool,
    pub fs_read: Option<FsReadTools>,
    pub doc_retriever: Option<DocRetrieverTool>,
    pub fs_write: Option<FsWriteTools>,
    pub add_attachment: Option<AddAttachmentTool>,
    #[cfg(feature = "pdf")]
    pub pdf_to_image: Option<PdfToImageTool>,
    #[cfg(feature = "pdf")]
    pub pdf_info: Option<PdfInfoTool>,
    #[cfg(feature = "pdf")]
    pub pdf_extract_text: Option<PdfExtractTextTool>,
    pub mcp_mgmt: McpTools,
    pub fetch_tool: Option<FetchTool>,
    pub shell_tools: Option<ShellTools>,
    pub git_tools: Option<GitTools>,
    pub search_tools: Option<SearchTools>,
    #[cfg(feature = "excel")]
    pub excel_read: Option<ReadExcelTool>,
    #[cfg(feature = "excel")]
    pub excel_write: Option<ExcelWriteTools>,
    #[cfg(feature = "docx")]
    pub docx_read: Option<ReadDocxTool>,
    #[cfg(feature = "docx")]
    pub docx_write: Option<WriteDocxTool>,
    #[cfg(feature = "pptx")]
    pub pptx_read: Option<ReadPptxTool>,
    #[cfg(feature = "pptx")]
    pub pptx_write: Option<WritePptxTool>,
    #[cfg(feature = "duckdb")]
    pub data_query: Option<DataQueryTools>,
    pub chart_tool: Option<CreateChartTool>,
    #[cfg(feature = "math-render")]
    pub typst_tool: Option<CompileTypstTool>,
    pub execute_code_tool: Option<ExecuteCodeTool>,
    pub remember_tool: Option<RememberTool>,
    pub save_skill_tool: Option<SaveSkillTool>,
    pub search_memory_tool: Option<SearchMemoryTool>,
    pub read_skill_tool: ReadSkillTool,
    pub search_web_tool: Option<SearchWebTool>,
    #[cfg(feature = "browser")]
    pub browser_tools: Option<BrowserTools>,
    pub browser_use_tool: Option<BrowserUseTool>,
    pub daytona_tool: Option<DaytonaTool>,
    pub list_agents_tool: ListAgentsTool,
    pub invoke_agent_tool: InvokeAgentTool,
    pub publish_module_tool: Option<PublishModuleTool>,
    pub ask_user_tool: Option<AskUserTool>,
}

/// Register `tool` unless the profile leaves its name out (ADR-0011 C11).
fn add<T: Tool + 'static>(
    builder: AgentBuilder<WithBuilderTools>,
    profile: Option<&'static ToolProfile>,
    tool: T,
) -> AgentBuilder<WithBuilderTools> {
    match profile {
        Some(profile) if !profile.allows(T::NAME) => builder,
        _ => builder.tool(tool),
    }
}

impl NativeTools {
    /// Register every collected tool on `builder` via typed `.tool()` calls,
    /// skipping whatever this agent's [`ToolProfile`] does not name.
    pub fn apply_to_builder(self, builder: AgentBuilder) -> AgentBuilder<WithBuilderTools> {
        let profile = self.tool_profile;
        // `dynamic_tools(&[])` is the only public way into the tool-carrying
        // builder state that registers nothing, and a profile may well drop
        // whichever tool would otherwise have made that transition.
        let mut b = builder.dynamic_tools(Vec::new());
        b = add(b, profile, self.list_tools);
        b = add(b, profile, self.write_todos_tool);
        b = add(b, profile, self.update_todo_tool);
        b = add(b, profile, self.verify_completion_tool);
        b = add(b, profile, self.list_agents_tool);
        b = add(b, profile, self.invoke_agent_tool);

        if let Some(t) = self.ask_user_tool {
            b = add(b, profile, t);
        }
        if let Some(t) = self.mcp_mgmt.list {
            b = add(b, profile, t);
        }
        if let Some((rf, rb, ld, gs)) = self.fs_read {
            b = add(b, profile, rf);
            b = add(b, profile, rb);
            b = add(b, profile, ld);
            b = add(b, profile, gs);
        }
        if let Some(dr) = self.doc_retriever {
            b = add(b, profile, dr);
        }
        if let Some((wf, fa, cd, df, mf, ad)) = self.fs_write {
            b = add(b, profile, wf);
            b = add(b, profile, fa);
            b = add(b, profile, cd);
            b = add(b, profile, df);
            b = add(b, profile, mf);
            b = add(b, profile, ad);
        }
        if let Some(t) = self.add_attachment {
            b = add(b, profile, t);
        }
        #[cfg(feature = "pdf")]
        if let Some(t) = self.pdf_to_image {
            b = add(b, profile, t);
        }
        #[cfg(feature = "pdf")]
        if let Some(t) = self.pdf_info {
            b = add(b, profile, t);
        }
        #[cfg(feature = "pdf")]
        if let Some(t) = self.pdf_extract_text {
            b = add(b, profile, t);
        }
        if let Some(t) = self.fetch_tool {
            b = add(b, profile, t);
        }
        if let Some((exec, set_env, cd, status)) = self.shell_tools {
            b = add(b, profile, exec);
            b = add(b, profile, set_env);
            b = add(b, profile, cd);
            b = add(b, profile, status);
        }
        if let Some((status, diff, log, add_tool, create_branch, switch_branch, commit)) =
            self.git_tools
        {
            b = add(b, profile, status);
            b = add(b, profile, diff);
            b = add(b, profile, log);
            b = add(b, profile, add_tool);
            b = add(b, profile, create_branch);
            b = add(b, profile, switch_branch);
            b = add(b, profile, commit);
        }
        if let Some((sc, ff, fd)) = self.search_tools {
            b = add(b, profile, sc);
            b = add(b, profile, ff);
            b = add(b, profile, fd);
        }
        #[cfg(feature = "excel")]
        if let Some(t) = self.excel_read {
            b = add(b, profile, t);
        }
        #[cfg(feature = "excel")]
        if let Some((wt, et)) = self.excel_write {
            b = add(b, profile, wt);
            b = add(b, profile, et);
        }
        #[cfg(feature = "docx")]
        if let Some(t) = self.docx_read {
            b = add(b, profile, t);
        }
        #[cfg(feature = "docx")]
        if let Some(t) = self.docx_write {
            b = add(b, profile, t);
        }
        #[cfg(feature = "pptx")]
        if let Some(t) = self.pptx_read {
            b = add(b, profile, t);
        }
        #[cfg(feature = "pptx")]
        if let Some(t) = self.pptx_write {
            b = add(b, profile, t);
        }
        #[cfg(feature = "duckdb")]
        if let Some((qt, dt, pt, fsd)) = self.data_query {
            b = add(b, profile, qt);
            b = add(b, profile, dt);
            b = add(b, profile, pt);
            b = add(b, profile, fsd);
        }
        if let Some(t) = self.chart_tool {
            b = add(b, profile, t);
        }
        #[cfg(feature = "math-render")]
        if let Some(t) = self.typst_tool {
            b = add(b, profile, t);
        }
        if let Some(t) = self.execute_code_tool {
            b = add(b, profile, t);
        }
        if let Some(t) = self.remember_tool {
            b = add(b, profile, t);
        }
        if let Some(t) = self.save_skill_tool {
            b = add(b, profile, t);
        }
        if let Some(t) = self.search_memory_tool {
            b = add(b, profile, t);
        }
        b = add(b, profile, self.read_skill_tool);
        if let Some(t) = self.search_web_tool {
            b = add(b, profile, t);
        }
        #[cfg(feature = "browser")]
        if let Some((nav, snap, shot, console, net, resize)) = self.browser_tools {
            b = add(b, profile, nav);
            b = add(b, profile, snap);
            b = add(b, profile, shot);
            b = add(b, profile, console);
            b = add(b, profile, net);
            b = add(b, profile, resize);
        }
        if let Some(t) = self.browser_use_tool {
            b = add(b, profile, t);
        }
        if let Some(t) = self.daytona_tool {
            b = add(b, profile, t);
        }
        if let Some(t) = self.publish_module_tool {
            b = add(b, profile, t);
        }
        b
    }
}

/// Construct a `NativeTools` struct with feature-gated fields.
///
/// All provider branches use the same field values (cloning from shared locals),
/// so this macro avoids repeating feature-gated field initialization per provider.
macro_rules! native_tools {
    (
        tool_profile: $tool_profile:expr,
        list_tools: $list_tools:expr,
        write_todos_tool: $write_todos_tool:expr,
        update_todo_tool: $update_todo_tool:expr,
        verify_completion_tool: $verify_completion_tool:expr,
        fs_read: $fs_read:expr,
        doc_retriever: $doc_retriever:expr,
        fs_write: $fs_write:expr,
        add_attachment: $add_attachment:expr,
        pdf_to_image: $pdf_to_image:expr,
        pdf_info: $pdf_info:expr,
        pdf_extract_text: $pdf_extract_text:expr,
        mcp_mgmt: $mcp_mgmt:expr,
        fetch_tool: $fetch_tool:expr,
        shell_tools: $shell_tools:expr,
        git_tools: $git_tools:expr,
        search_tools: $search_tools:expr,
        excel_read: $excel_read:expr,
        excel_write: $excel_write:expr,
        docx_read: $docx_read:expr,
        docx_write: $docx_write:expr,
        pptx_read: $pptx_read:expr,
        pptx_write: $pptx_write:expr,
        data_query: $data_query:expr,
        chart_tool: $chart_tool:expr,
        typst_tool: $typst_tool:expr,
        execute_code_tool: $execute_code_tool:expr,
        remember_tool: $remember_tool:expr,
        save_skill_tool: $save_skill_tool:expr,
        search_memory_tool: $search_memory_tool:expr,
        read_skill_tool: $read_skill_tool:expr,
        search_web_tool: $search_web_tool:expr,
        browser_tools: $browser_tools:expr,
        browser_use_tool: $browser_use_tool:expr,
        daytona_tool: $daytona_tool:expr,
        list_agents_tool: $list_agents_tool:expr,
        invoke_agent_tool: $invoke_agent_tool:expr,
        publish_module_tool: $publish_module_tool:expr,
        ask_user_tool: $ask_user_tool:expr $(,)?
    ) => {
        NativeTools {
            tool_profile: $tool_profile,
            list_tools: $list_tools,
            write_todos_tool: $write_todos_tool,
            update_todo_tool: $update_todo_tool,
            verify_completion_tool: $verify_completion_tool,
            fs_read: $fs_read,
            doc_retriever: $doc_retriever,
            fs_write: $fs_write,
            add_attachment: $add_attachment,
            #[cfg(feature = "pdf")]
            pdf_to_image: $pdf_to_image,
            #[cfg(feature = "pdf")]
            pdf_info: $pdf_info,
            #[cfg(feature = "pdf")]
            pdf_extract_text: $pdf_extract_text,
            mcp_mgmt: $mcp_mgmt,
            fetch_tool: $fetch_tool,
            shell_tools: $shell_tools,
            git_tools: $git_tools,
            search_tools: $search_tools,
            #[cfg(feature = "excel")]
            excel_read: $excel_read,
            #[cfg(feature = "excel")]
            excel_write: $excel_write,
            #[cfg(feature = "docx")]
            docx_read: $docx_read,
            #[cfg(feature = "docx")]
            docx_write: $docx_write,
            #[cfg(feature = "pptx")]
            pptx_read: $pptx_read,
            #[cfg(feature = "pptx")]
            pptx_write: $pptx_write,
            #[cfg(feature = "duckdb")]
            data_query: $data_query,
            chart_tool: $chart_tool,
            #[cfg(feature = "math-render")]
            typst_tool: $typst_tool,
            execute_code_tool: $execute_code_tool,
            remember_tool: $remember_tool,
            save_skill_tool: $save_skill_tool,
            search_memory_tool: $search_memory_tool,
            read_skill_tool: $read_skill_tool,
            search_web_tool: $search_web_tool,
            #[cfg(feature = "browser")]
            browser_tools: $browser_tools,
            browser_use_tool: $browser_use_tool,
            daytona_tool: $daytona_tool,
            list_agents_tool: $list_agents_tool,
            invoke_agent_tool: $invoke_agent_tool,
            publish_module_tool: $publish_module_tool,
            ask_user_tool: $ask_user_tool,
        }
    };
}

pub(super) use native_tools;
