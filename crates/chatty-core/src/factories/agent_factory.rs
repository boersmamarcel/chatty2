use anyhow::{Context, Result, anyhow};
use rig::agent::Agent;
use rig::client::CompletionClient;
use rig::tool::ToolDyn;
use std::sync::OnceLock;

use crate::auth::{AzureTokenCache, azure_auth};
use crate::sandbox::{SandboxConfig, SandboxManager};
use crate::services::filesystem_service::FileSystemService;
use crate::services::git_service::GitService;
use crate::services::search_service::CodeSearchService;
use crate::services::shell_service::ShellSession;
use crate::tools::{
    AddAttachmentTool, AddMcpTool, ApplyDiffTool, CompileTypstTool, CreateChartTool,
    CreateDirectoryTool, DeleteFileTool, DeleteMcpTool, DescribeDataTool, EditExcelTool,
    EditMcpTool, ExecuteCodeTool, FetchTool, FindDefinitionTool, FindFilesTool, GitAddTool,
    GitCommitTool, GitCreateBranchTool, GitDiffTool, GitLogTool, GitStatusTool,
    GitSwitchBranchTool, GlobSearchTool, ListDirectoryTool, ListMcpTool, ListToolsTool,
    MoveFileTool, PdfExtractTextTool, PdfInfoTool, PdfToImageTool, PendingArtifacts, QueryDataTool,
    ReadBinaryTool, ReadExcelTool, ReadFileTool, SearchCodeTool, ShellCdTool, ShellExecuteTool,
    ShellSetEnvTool, ShellStatusTool, WriteExcelTool, WriteFileTool,
};
use crate::settings::models::models_store::{AZURE_DEFAULT_API_VERSION, ModelConfig};
use crate::settings::models::providers_store::{AzureAuthMethod, ProviderConfig, ProviderType};

static AZURE_TOKEN_CACHE: OnceLock<Option<AzureTokenCache>> = OnceLock::new();

/// Filesystem read tool set
type FsReadTools = (
    ReadFileTool,
    ReadBinaryTool,
    ListDirectoryTool,
    GlobSearchTool,
);

/// Filesystem write tool set
type FsWriteTools = (
    WriteFileTool,
    CreateDirectoryTool,
    DeleteFileTool,
    MoveFileTool,
    ApplyDiffTool,
);

/// Shell session tool set (all four shell tools)
type ShellTools = (
    ShellExecuteTool,
    ShellSetEnvTool,
    ShellCdTool,
    ShellStatusTool,
);

/// Git integration tool set (seven git tools)
type GitTools = (
    GitStatusTool,
    GitDiffTool,
    GitLogTool,
    GitAddTool,
    GitCreateBranchTool,
    GitSwitchBranchTool,
    GitCommitTool,
);

/// Code search tool set (search_code, find_files, find_definition)
type SearchTools = (SearchCodeTool, FindFilesTool, FindDefinitionTool);

/// Excel tool sets (gated on filesystem read/write settings)
type ExcelWriteTools = (WriteExcelTool, EditExcelTool);

/// DuckDB data query tools (gated on filesystem_read_enabled)
type DataQueryTools = (QueryDataTool, DescribeDataTool);

/// All four MCP management tools bundled together.
///
/// All four are gated on the same `mcp_service_tool_enabled` setting, so they
/// are always constructed (or not) as a unit.
struct McpTools {
    add: Option<AddMcpTool>,
    delete: Option<DeleteMcpTool>,
    edit: Option<EditMcpTool>,
    list: Option<ListMcpTool>,
}

impl McpTools {
    fn none() -> Self {
        Self {
            add: None,
            delete: None,
            edit: None,
            list: None,
        }
    }

    fn is_enabled(&self) -> bool {
        self.add.is_some()
    }
}

/// Deduplicate MCP tools by name across all servers.
///
/// When multiple MCP servers are configured, they may provide tools with the same name.
/// LLM providers (Anthropic, OpenAI, etc.) require unique tool names, so this function
/// deduplicates by keeping the first occurrence of each tool name and logging skipped duplicates.
fn deduplicate_mcp_tools(
    mcp_tools: Vec<(String, Vec<rmcp::model::Tool>, rmcp::service::ServerSink)>,
) -> Vec<(String, Vec<rmcp::model::Tool>, rmcp::service::ServerSink)> {
    use std::collections::HashSet;

    let mut seen_tool_names = HashSet::new();
    let mut result = Vec::new();

    for (server_name, tools, sink) in mcp_tools {
        let mut deduped_tools = Vec::new();
        let mut skipped_count = 0;
        let total_tools = tools.len();

        for tool in tools {
            if seen_tool_names.insert(tool.name.clone()) {
                // New tool name, keep it
                deduped_tools.push(tool);
            } else {
                // Duplicate tool name, skip it
                tracing::warn!(
                    server = %server_name,
                    tool_name = %tool.name,
                    "Skipping duplicate tool name from MCP server"
                );
                skipped_count += 1;
            }
        }

        if skipped_count > 0 {
            tracing::info!(
                server = %server_name,
                total = total_tools,
                kept = deduped_tools.len(),
                skipped = skipped_count,
                "Deduplicated tools from MCP server"
            );
        }

        if !deduped_tools.is_empty() {
            result.push((server_name, deduped_tools, sink));
        }
    }

    result
}

macro_rules! build_with_mcp_tools {
    ($builder:expr, $mcp_tools:expr) => {{
        match $mcp_tools {
            Some(tools_list) => {
                let deduped = deduplicate_mcp_tools(tools_list);
                let mut iter = deduped
                    .into_iter()
                    .filter(|(_name, t, _sink)| !t.is_empty());
                if let Some((_first_name, first_tools, first_sink)) = iter.next() {
                    let mut b = $builder.rmcp_tools(first_tools, first_sink);
                    for (_name, tools, sink) in iter {
                        b = b.rmcp_tools(tools, sink);
                    }
                    b.build()
                } else {
                    $builder.build()
                }
            }
            None => $builder.build(),
        }
    }};
}

/// Recursively strip `"format"` fields from a JSON Schema object.
///
/// OpenAI strict-mode function calling does not support the `"format"` keyword
/// (e.g., `"format": "uri"`). MCP tool schemas may include these, so we strip
/// them before sending to OpenAI / Azure OpenAI.
///
/// TODO(#127): Remove once rig-core's `sanitize_schema()` strips `"format"`.
fn strip_format_from_schema(schema: &mut serde_json::Map<String, serde_json::Value>) {
    schema.remove("format");

    if let Some(serde_json::Value::Object(props)) = schema.get_mut("properties") {
        for prop_value in props.values_mut() {
            if let serde_json::Value::Object(prop_obj) = prop_value {
                strip_format_from_schema(prop_obj);
            }
        }
    }
    if let Some(serde_json::Value::Object(items)) = schema.get_mut("items") {
        strip_format_from_schema(items);
    }
    for key in ["anyOf", "oneOf", "allOf"] {
        if let Some(serde_json::Value::Array(variants)) = schema.get_mut(key) {
            for variant in variants.iter_mut() {
                if let serde_json::Value::Object(obj) = variant {
                    strip_format_from_schema(obj);
                }
            }
        }
    }
    if let Some(serde_json::Value::Object(defs)) = schema.get_mut("$defs") {
        for def in defs.values_mut() {
            if let serde_json::Value::Object(def_obj) = def {
                strip_format_from_schema(def_obj);
            }
        }
    }
}

/// Sanitize MCP tool schemas for OpenAI compatibility.
///
/// Strips unsupported JSON Schema keywords (like `"format"`) that OpenAI's
/// strict-mode function calling rejects.
///
/// TODO(#127): Remove once rig-core's `sanitize_schema()` strips `"format"`.
fn sanitize_mcp_tools_for_openai(
    mcp_tools: Option<Vec<(String, Vec<rmcp::model::Tool>, rmcp::service::ServerSink)>>,
) -> Option<Vec<(String, Vec<rmcp::model::Tool>, rmcp::service::ServerSink)>> {
    mcp_tools.map(|servers| {
        servers
            .into_iter()
            .map(|(name, tools, sink)| {
                let sanitized_tools = tools
                    .into_iter()
                    .map(|mut tool| {
                        let mut schema = (*tool.input_schema).clone();
                        strip_format_from_schema(&mut schema);
                        tool.input_schema = std::sync::Arc::new(schema);
                        tool
                    })
                    .collect();
                (name, sanitized_tools, sink)
            })
            .collect()
    })
}

/// Collect all optional native tools into a `Vec<Box<dyn ToolDyn>>`.
///
/// Replaces the former 16-branch `build_agent_with_tools!` macro. Adding a new
/// optional tool only requires one new `if let Some` block here — no combinatorial
/// branching.
#[allow(clippy::too_many_arguments)]
fn collect_tools(
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
) -> Vec<Box<dyn ToolDyn>> {
    let mut tools: Vec<Box<dyn ToolDyn>> = Vec::new();
    tools.push(Box::new(list_tools)); // always present
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
    tools
}

/// Enum-based agent wrapper for multi-provider support
#[derive(Clone)]
pub enum AgentClient {
    Anthropic(Agent<rig::providers::anthropic::completion::CompletionModel>),
    OpenAI(Agent<rig::providers::openai::responses_api::ResponsesCompletionModel>),
    Gemini(Agent<rig::providers::gemini::completion::CompletionModel>),
    Mistral(Agent<rig::providers::mistral::completion::CompletionModel>),
    Ollama(Agent<rig::providers::ollama::CompletionModel>),
    AzureOpenAI(Agent<rig::providers::azure::CompletionModel>),
}

impl AgentClient {
    /// Create AgentClient from ModelConfig and ProviderConfig with optional MCP tools, shell execution, and filesystem tools
    #[allow(clippy::too_many_arguments)]
    pub async fn from_model_config_with_tools(
        model_config: &ModelConfig,
        provider_config: &ProviderConfig,
        mcp_tools: Option<Vec<(String, Vec<rmcp::model::Tool>, rmcp::service::ServerSink)>>,
        exec_settings: Option<crate::settings::models::ExecutionSettingsModel>,
        pending_approvals: Option<
            crate::models::execution_approval_store::PendingApprovals,
        >,
        pending_write_approvals: Option<
            crate::models::write_approval_store::PendingWriteApprovals,
        >,
        pending_artifacts: Option<PendingArtifacts>,
        shell_session: Option<std::sync::Arc<ShellSession>>,
        user_secrets: Vec<(String, String)>,
        theme_colors: Option<[String; 5]>,
    ) -> Result<(Self, Option<std::sync::Arc<ShellSession>>)> {
        let api_key = provider_config.api_key.clone();
        let base_url = provider_config.base_url.clone();

        // Extract secret key names before user_secrets is moved into ShellSession.
        // Used to augment the preamble so the LLM knows which env vars are available.
        let secret_key_names: Vec<String> = user_secrets.iter().map(|(k, _)| k.clone()).collect();

        // Ensure shell session exists when execution is enabled (factory-level fallback).
        // This guarantees shell tools are created regardless of how the caller constructed
        // the session parameter.
        let shell_session = if shell_session.is_some() {
            shell_session
        } else {
            exec_settings.as_ref().and_then(|settings| {
                if settings.enabled {
                    tracing::info!(
                        workspace = ?settings.workspace_dir,
                        "Creating shell session in factory (caller did not provide one)"
                    );
                    Some(std::sync::Arc::new(ShellSession::with_secrets(
                        settings.workspace_dir.clone(),
                        settings.timeout_seconds,
                        settings.max_output_bytes,
                        settings.network_isolation,
                        user_secrets,
                    )))
                } else {
                    tracing::info!(
                        enabled = settings.enabled,
                        workspace = ?settings.workspace_dir,
                        "Shell session not created: execution disabled in settings"
                    );
                    None
                }
            })
        };

        // Save a clone to return to the caller so it can be stored on the Conversation
        let shell_session_out = shell_session.clone();

        // Create shell session tools if execution is enabled and a session is provided
        let shell_tools: Option<ShellTools> =
            if let (Some(session), Some(settings), Some(approvals)) =
                (&shell_session, &exec_settings, &pending_approvals)
            {
                if settings.enabled {
                    tracing::info!("Shell session tools enabled");
                    Some((
                        ShellExecuteTool::new(session.clone(), settings.clone(), approvals.clone()),
                        ShellSetEnvTool::new(session.clone(), settings.clone()),
                        ShellCdTool::new(session.clone(), settings.clone()),
                        ShellStatusTool::new(session.clone()),
                    ))
                } else {
                    tracing::info!("Shell session tools skipped: execution disabled");
                    None
                }
            } else {
                tracing::info!(
                    has_shell_session = shell_session.is_some(),
                    has_exec_settings = exec_settings.is_some(),
                    has_pending_approvals = pending_approvals.is_some(),
                    "Shell session tools skipped: missing required components"
                );
                None
            };

        // Create filesystem tools if a workspace directory is configured
        let mut add_attachment_tool: Option<AddAttachmentTool> = None;
        let mut pdf_to_image_tool: Option<PdfToImageTool> = None;
        let mut pdf_info_tool: Option<PdfInfoTool> = None;
        let mut pdf_extract_text_tool: Option<PdfExtractTextTool> = None;
        let mut search_tools: Option<SearchTools> = None;
        #[allow(clippy::type_complexity)]
        let (fs_read_tools, fs_write_tools, excel_read_tool, excel_write_tools, data_query_tools): (
            Option<FsReadTools>,
            Option<FsWriteTools>,
            Option<ReadExcelTool>,
            Option<ExcelWriteTools>,
            Option<DataQueryTools>,
        ) = match exec_settings
            .as_ref()
            .and_then(|s| s.workspace_dir.as_ref())
        {
            Some(workspace_dir) => match FileSystemService::new(workspace_dir).await {
                Ok(service) => {
                    let service = std::sync::Arc::new(service);

                    // Read tools - check both workspace_dir AND filesystem_read_enabled
                    let read_tools = if exec_settings
                        .as_ref()
                        .map(|s| s.filesystem_read_enabled)
                        .unwrap_or(false)
                    {
                        tracing::info!(workspace = %workspace_dir, "Filesystem read tools enabled");

                        // Create add_attachment tool alongside read tools.
                        // Available for all models — the tool displays files inline
                        // in the chat and does not require multimodal model support.
                        if let Some(ref artifacts) = pending_artifacts {
                            add_attachment_tool =
                                Some(AddAttachmentTool::new(service.clone(), artifacts.clone()));
                            pdf_to_image_tool =
                                Some(PdfToImageTool::new(service.clone(), artifacts.clone()));
                        }

                        // PDF tools that don't need PendingArtifacts
                        pdf_info_tool = Some(PdfInfoTool::new(service.clone()));
                        pdf_extract_text_tool = Some(PdfExtractTextTool::new(service.clone()));

                        // Create code search tools alongside filesystem read tools
                        match CodeSearchService::new(workspace_dir) {
                            Ok(search_service) => {
                                let search_service = std::sync::Arc::new(search_service);
                                tracing::info!(workspace = %workspace_dir, "Code search tools enabled");
                                search_tools = Some((
                                    SearchCodeTool::new(search_service.clone()),
                                    FindFilesTool::new(search_service.clone()),
                                    FindDefinitionTool::new(search_service.clone()),
                                ));
                            }
                            Err(e) => {
                                tracing::warn!(error = ?e, workspace = %workspace_dir, "Failed to initialize code search tools");
                            }
                        }

                        Some((
                            ReadFileTool::new(service.clone()),
                            ReadBinaryTool::new(service.clone()),
                            ListDirectoryTool::new(service.clone()),
                            GlobSearchTool::new(service.clone()),
                        ))
                    } else {
                        tracing::info!(workspace = %workspace_dir, "Filesystem read tools disabled");
                        None
                    };

                    // Write tools - check both workspace_dir AND filesystem_write_enabled
                    let write_tools = if exec_settings
                        .as_ref()
                        .map(|s| s.filesystem_write_enabled)
                        .unwrap_or(false)
                    {
                        tracing::info!(workspace = %workspace_dir, "Filesystem write tools enabled");
                        pending_write_approvals.as_ref().map(|approvals| {
                            (
                                WriteFileTool::new(service.clone(), approvals.clone()),
                                CreateDirectoryTool::new(service.clone()),
                                DeleteFileTool::new(service.clone(), approvals.clone()),
                                MoveFileTool::new(service.clone(), approvals.clone()),
                                ApplyDiffTool::new(service.clone(), approvals.clone()),
                            )
                        })
                    } else {
                        tracing::info!(workspace = %workspace_dir, "Filesystem write tools disabled");
                        None
                    };

                    // Excel read tool - gated on filesystem_read_enabled
                    let excel_read_tool = if read_tools.is_some() {
                        tracing::info!(workspace = %workspace_dir, "Excel read tool enabled");
                        Some(ReadExcelTool::new(service.clone()))
                    } else {
                        None
                    };

                    // Excel write tools - gated on filesystem_write_enabled
                    let excel_write_tools = if write_tools.is_some() {
                        pending_write_approvals.as_ref().map(|approvals| {
                            tracing::info!(workspace = %workspace_dir, "Excel write tools enabled");
                            (
                                WriteExcelTool::new(service.clone(), approvals.clone()),
                                EditExcelTool::new(service.clone(), approvals.clone()),
                            )
                        })
                    } else {
                        None
                    };

                    // Data query tools - gated on filesystem_read_enabled
                    let data_query_tools = if read_tools.is_some() {
                        tracing::info!(workspace = %workspace_dir, "Data query tools enabled");
                        Some((
                            QueryDataTool::new(service.clone()),
                            DescribeDataTool::new(service.clone()),
                        ))
                    } else {
                        None
                    };

                    (read_tools, write_tools, excel_read_tool, excel_write_tools, data_query_tools)
                }
                Err(e) => {
                    tracing::warn!(error = ?e, workspace = %workspace_dir, "Failed to initialize filesystem tools");
                    (None, None, None, None, None)
                }
            },
            None => (None, None, None, None, None),
        };

        // Extract MCP tool metadata so list_tools can report them to the model
        let mcp_tool_info: Vec<(String, String, String)> = mcp_tools
            .as_ref()
            .map(|tools_list| {
                tracing::info!(
                    server_count = tools_list.len(),
                    "Extracting MCP tool metadata for list_tools"
                );
                tools_list
                    .iter()
                    .flat_map(|(server_name, tools, _sink)| {
                        tracing::info!(
                            server = %server_name,
                            tool_count = tools.len(),
                            "Processing MCP tools from server"
                        );
                        let server_name = server_name.clone();
                        tools.iter().map(move |tool| {
                            tracing::debug!(
                                server = %server_name,
                                tool_name = %tool.name,
                                "Adding MCP tool to metadata list"
                            );
                            (
                                server_name.clone(),
                                tool.name.to_string(),
                                tool.description
                                    .as_deref()
                                    .unwrap_or("No description available")
                                    .to_string(),
                            )
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        tracing::info!(
            mcp_tool_count = mcp_tool_info.len(),
            "Total MCP tools registered with list_tools"
        );

        // Create MCP management tools (all four are gated on the same setting)
        let mcp_mgmt_tools = {
            let enabled = exec_settings
                .as_ref()
                .map(|s| s.mcp_service_tool_enabled)
                .unwrap_or(false);
            if enabled {
                let (add, delete, edit) = match (
                    crate::MCP_UPDATE_SENDER.get().cloned(),
                    crate::MCP_SERVICE.get().cloned(),
                ) {
                    (Some(sender), Some(service)) => (
                        AddMcpTool::new_with_services(
                            crate::MCP_REPOSITORY.clone(),
                            sender.clone(),
                            service.clone(),
                        ),
                        DeleteMcpTool::new_with_services(
                            crate::MCP_REPOSITORY.clone(),
                            sender.clone(),
                            service.clone(),
                        ),
                        EditMcpTool::new_with_services(
                            crate::MCP_REPOSITORY.clone(),
                            sender,
                            service,
                        ),
                    ),
                    _ => {
                        tracing::warn!(
                            "MCP_UPDATE_SENDER or MCP_SERVICE not initialized; \
                             MCP tools created without live services"
                        );
                        (
                            AddMcpTool::new(crate::MCP_REPOSITORY.clone()),
                            DeleteMcpTool::new(crate::MCP_REPOSITORY.clone()),
                            EditMcpTool::new(crate::MCP_REPOSITORY.clone()),
                        )
                    }
                };
                McpTools {
                    add: Some(add),
                    delete: Some(delete),
                    edit: Some(edit),
                    list: Some(ListMcpTool::new(crate::MCP_REPOSITORY.clone())),
                }
            } else {
                tracing::info!("MCP management tools disabled by execution settings");
                McpTools::none()
            }
        };

        // Create fetch tool if enabled in settings
        let fetch_tool = if exec_settings
            .as_ref()
            .map(|s| s.fetch_enabled)
            .unwrap_or(true)
        {
            let workspace = exec_settings
                .as_ref()
                .and_then(|s| s.workspace_dir.as_ref())
                .map(std::path::PathBuf::from);
            tracing::info!(?workspace, "Fetch tool enabled");
            Some(FetchTool::new(workspace))
        } else {
            tracing::info!("Fetch tool disabled by execution settings");
            None
        };

        // Create git tools if enabled and workspace is a git repository
        let git_tools: Option<GitTools> = if exec_settings
            .as_ref()
            .map(|s| s.git_enabled)
            .unwrap_or(false)
        {
            match exec_settings
                .as_ref()
                .and_then(|s| s.workspace_dir.as_ref())
            {
                Some(workspace_dir) => match GitService::new(workspace_dir).await {
                    Ok(service) => {
                        let service = std::sync::Arc::new(service);
                        let approval_mode = exec_settings
                            .as_ref()
                            .map(|s| s.approval_mode.clone())
                            .unwrap_or_default();
                        let approvals = pending_approvals.clone().unwrap_or_else(|| {
                            std::sync::Arc::new(std::sync::Mutex::new(
                                std::collections::HashMap::new(),
                            ))
                        });

                        tracing::info!(workspace = %workspace_dir, "Git tools enabled");
                        Some((
                            GitStatusTool::new(service.clone()),
                            GitDiffTool::new(service.clone()),
                            GitLogTool::new(service.clone()),
                            GitAddTool::new(
                                service.clone(),
                                approval_mode.clone(),
                                approvals.clone(),
                            ),
                            GitCreateBranchTool::new(
                                service.clone(),
                                approval_mode.clone(),
                                approvals.clone(),
                            ),
                            GitSwitchBranchTool::new(
                                service.clone(),
                                approval_mode.clone(),
                                approvals.clone(),
                            ),
                            GitCommitTool::new(service, approval_mode, approvals),
                        ))
                    }
                    Err(e) => {
                        tracing::warn!(
                            error = ?e,
                            workspace = %workspace_dir,
                            "Failed to initialize git tools (workspace may not be a git repository)"
                        );
                        None
                    }
                },
                None => {
                    tracing::info!("Git tools skipped: no workspace directory configured");
                    None
                }
            }
        } else {
            tracing::info!("Git tools disabled by execution settings");
            None
        };

        // Chart tool is always available (no service dependencies).
        // Pass workspace_dir so relative save_path values resolve correctly.
        // Pass theme_colors so saved PNG files match the inline chart appearance.
        let chart_tool: Option<CreateChartTool> = Some(CreateChartTool::new(
            exec_settings.as_ref().and_then(|s| s.workspace_dir.clone()),
            theme_colors,
        ));

        // Typst compile tool - gated on filesystem_write_enabled (writes PDF files to disk).
        let typst_tool: Option<CompileTypstTool> = if exec_settings
            .as_ref()
            .map(|s| s.filesystem_write_enabled)
            .unwrap_or(false)
        {
            tracing::info!("Typst compile tool enabled");
            Some(CompileTypstTool::new(
                exec_settings.as_ref().and_then(|s| s.workspace_dir.clone()),
            ))
        } else {
            None
        };

        // Docker code execution tool - gated on docker_code_execution_enabled
        let execute_code_tool: Option<ExecuteCodeTool> = if exec_settings
            .as_ref()
            .map(|s| s.docker_code_execution_enabled)
            .unwrap_or(false)
        {
            tracing::info!("Docker code execution tool enabled");
            let sandbox_config = SandboxConfig {
                timeout_secs: exec_settings
                    .as_ref()
                    .map(|s| s.timeout_seconds as u64)
                    .unwrap_or(30),
                network: !exec_settings
                    .as_ref()
                    .map(|s| s.network_isolation)
                    .unwrap_or(true),
                workspace_path: exec_settings.as_ref().and_then(|s| s.workspace_dir.clone()),
                ..SandboxConfig::default()
            };
            let manager = std::sync::Arc::new(SandboxManager::new(sandbox_config));
            Some(ExecuteCodeTool::new(manager))
        } else {
            tracing::info!("Docker code execution tool disabled by execution settings");
            None
        };

        // Create list_tools tool (always available, shows native + MCP tools)
        let list_tools = ListToolsTool::new_with_config(
            fs_read_tools.is_some(),
            fs_write_tools.is_some(),
            mcp_mgmt_tools.is_enabled(),
            fetch_tool.is_some(),
            shell_tools.is_some(),
            git_tools.is_some(),
            search_tools.is_some(),
            add_attachment_tool.is_some(),
            excel_read_tool.is_some(),
            excel_write_tools.is_some(),
            pdf_to_image_tool.is_some(),
            pdf_info_tool.is_some(),
            pdf_extract_text_tool.is_some(),
            data_query_tools.is_some(),
            typst_tool.is_some(),
            execute_code_tool.is_some(),
            mcp_tool_info,
        );

        // Build a compact tool capability summary so the LLM knows what it can do
        // from the very first turn — without requiring the user to ask or the model
        // to call list_tools first.
        let mut tool_sections: Vec<String> = Vec::new();

        if fetch_tool.is_some() {
            tool_sections
                .push("- **fetch**: Fetch web URLs and return readable text content".to_string());
        }
        if shell_tools.is_some() {
            tool_sections.push(
                "- **shell_execute / shell_cd / shell_set_env / shell_status**: \
                 Run any shell/terminal command in a persistent session that preserves \
                 working directory and environment variables across calls. \
                 Prefer this over asking the user to run commands manually."
                    .to_string(),
            );
        }
        if fs_read_tools.is_some() {
            tool_sections.push(
                "- **read_file / read_binary / list_directory / glob_search**: \
                 Read files and explore the workspace directory."
                    .to_string(),
            );
        }
        if fs_write_tools.is_some() {
            tool_sections.push(
                "- **write_file / apply_diff / create_directory / delete_file / move_file**: \
                 Create, edit, and manage files in the workspace. \
                 Use apply_diff for targeted edits to existing files."
                    .to_string(),
            );
        }
        if search_tools.is_some() {
            tool_sections.push(
                "- **search_code / find_files / find_definition**: \
                 Search for patterns, files, and symbol definitions in the workspace."
                    .to_string(),
            );
        }
        if git_tools.is_some() {
            tool_sections.push(
                "- **git_status / git_diff / git_log / git_add / git_commit / \
                 git_create_branch / git_switch_branch**: \
                 Inspect and manage git history and branches."
                    .to_string(),
            );
        }
        if add_attachment_tool.is_some() {
            tool_sections.push(
                "- **add_attachment**: Display an image or PDF inline in the chat response. \
                 Useful for showing generated plots, screenshots, or documents."
                    .to_string(),
            );
        }
        // Chart tool is always available (no filesystem/service dependencies)
        tool_sections.push(
            "- **create_chart**: Create and display a chart inline in the chat response. \
             Supports bar (with value labels), line, pie, donut, area, and candlestick charts. \
             Use this to visualize data for the user."
                .to_string(),
        );
        if typst_tool.is_some() {
            tool_sections.push(
                "- **compile_typst**: Compile Typst markup into a PDF file saved to disk. \
                 Use for generating formatted documents: reports, papers, documents with math, \
                 tables, headings, and code blocks. Typst syntax is markdown-like — \
                 `= Heading`, `*bold*`, `_italic_`, `$ math $`, `#table(...)`, etc."
                    .to_string(),
            );
        }
        if excel_read_tool.is_some() || excel_write_tools.is_some() {
            let mut excel_desc = Vec::new();
            if excel_read_tool.is_some() {
                excel_desc.push("**read_excel**");
            }
            if excel_write_tools.is_some() {
                excel_desc.push("**write_excel** / **edit_excel**");
            }
            tool_sections.push(format!(
                "- {}: Read, create, and edit Excel spreadsheets (.xlsx, .xls, .xlsm, .xlsb, .ods). \
                 Supports cell data, formatting, formulas, merged cells, and auto-filters.",
                excel_desc.join(" / ")
            ));
        }
        if pdf_to_image_tool.is_some() || pdf_info_tool.is_some() || pdf_extract_text_tool.is_some()
        {
            let mut pdf_desc = String::from("- **PDF tools**:");
            if pdf_info_tool.is_some() {
                pdf_desc.push_str(" `pdf_info` (page count, dimensions, metadata),");
            }
            if pdf_extract_text_tool.is_some() {
                pdf_desc.push_str(" `pdf_extract_text` (extract text from pages),");
            }
            if pdf_to_image_tool.is_some() {
                pdf_desc.push_str(
                    " `pdf_to_image` (render pages as PNG images for visual inspection),",
                );
            }
            // Remove trailing comma and add period
            if pdf_desc.ends_with(',') {
                pdf_desc.pop();
            }
            pdf_desc.push('.');
            tool_sections.push(pdf_desc);
        }
        if data_query_tools.is_some() {
            tool_sections.push(
                "- **query_data / describe_data**: Run SQL queries against local Parquet, CSV, \
                 and JSON files using DuckDB. Use `describe_data` to inspect schema first, \
                 then `query_data` for analytical SQL (aggregations, joins, window functions)."
                    .to_string(),
            );
        }
        if mcp_mgmt_tools.is_enabled() {
            tool_sections.push(
                "- **list_mcp_services / add_mcp_service / edit_mcp_service / delete_mcp_service**: \
                 Manage MCP server configurations."
                    .to_string(),
            );
        }
        if execute_code_tool.is_some() {
            tool_sections.push(
                "- **execute_code**: Execute code in an isolated Docker sandbox. \
                 Supports python, javascript, typescript, rust, and bash. \
                 State (variables, installed packages) persists throughout the conversation. \
                 No network access. Use this for running code snippets, \
                 data analysis, or verifying solutions."
                    .to_string(),
            );
        }
        // Always present
        tool_sections.push(
            "- **list_tools**: Call this at any time to get the full, up-to-date list of \
             available tools with their exact names and descriptions."
                .to_string(),
        );

        let tool_summary = if tool_sections.is_empty() {
            String::new()
        } else {
            format!(
                "\n\n## Available Tools\n\
                 You have access to the following tools. Use them proactively to help the user \
                 instead of asking them to do things manually:\n\n{}",
                tool_sections.join("\n")
            )
        };

        // Formatting capabilities the app always renders, regardless of tool settings.
        let formatting_guide = "\n\n## Formatting Capabilities\n\
             \n### Math (Typst/LaTeX)\n\
             The app renders math expressions natively. Use any of these delimiters:\n\
             - Inline math: `$...$` or `\\(...\\)` — e.g. `$E = mc^2$`\n\
             - Block (display) math: `$$...$$` on its own line, `\\[...\\]`, or fenced blocks \
             with ` ```math ` or ` ```latex `\n\
             - LaTeX environments: `\\begin{equation}`, `\\begin{align}`, `\\begin{gather}`, \
             `\\begin{matrix}`, `\\begin{cases}`, and all standard starred variants\n\
             Math is compiled via MiTeX → Typst → SVG and cached; use standard LaTeX notation freely.\n\
             \n### Mermaid Diagrams\n\
             The app renders Mermaid diagrams natively in ` ```mermaid ` fenced code blocks.\n\
             Supported diagram types: flowchart, sequence, class, state, ER, gantt, pie, mindmap, \
             timeline, git graph, C4, architecture, and more.\n\
             Use mermaid diagrams to visualize workflows, architectures, relationships, and processes.\n\
             Example: ` ```mermaid\\nflowchart TD\\n  A[Start] --> B{Decision}\\n  B -->|Yes| C[OK]\\n  B -->|No| D[Cancel]\\n``` `\n\
             \n### Thinking / Reasoning\n\
             Wrap internal reasoning in `<thinking>...</thinking>`, `<think>...</think>`, or `<thought>...</thought>` tags. \
             The app renders these as a visually distinct, collapsible block so the user can inspect \
             your reasoning without it cluttering the main response. \
             Use thinking blocks for multi-step reasoning, planning, or working through a problem \
             before giving a final answer.";

        // Augment preamble with tool summary, formatting guide, and available secret key names.
        let preamble = {
            let mut p = model_config.preamble.clone();
            p.push_str(&tool_summary);
            p.push_str(formatting_guide);
            if !secret_key_names.is_empty() {
                p.push_str(&format!(
                    "\n\nThe following environment variables with sensitive information are \
                     pre-loaded in the shell session: {}. When generating code that needs \
                     these values, access them directly (e.g., os.environ[\"KEY\"] in Python, \
                     $KEY in bash). Do not ask the user to provide these values.",
                    secret_key_names.join(", ")
                ));
            }
            p
        };

        match &provider_config.provider_type {
            ProviderType::Anthropic => {
                let key = api_key
                    .ok_or_else(|| anyhow!("API key not configured for Anthropic provider"))?;

                let client = rig::providers::anthropic::Client::new(&key)?;
                let mut builder = client
                    .agent(&model_config.model_identifier)
                    .preamble(&preamble)
                    .temperature(model_config.temperature as f64);

                if let Some(max_tokens) = model_config.max_tokens {
                    builder = builder.max_tokens(max_tokens as u64);
                }

                // Build with all tools
                let tool_vec = collect_tools(
                    list_tools,
                    fs_read_tools,
                    fs_write_tools,
                    add_attachment_tool.clone(),
                    pdf_to_image_tool.clone(),
                    pdf_info_tool.clone(),
                    pdf_extract_text_tool.clone(),
                    mcp_mgmt_tools,
                    fetch_tool.clone(),
                    shell_tools,
                    git_tools,
                    search_tools,
                    excel_read_tool.clone(),
                    excel_write_tools.clone(),
                    data_query_tools.clone(),
                    chart_tool.clone(),
                    typst_tool.clone(),
                    execute_code_tool.clone(),
                );
                let agent = build_with_mcp_tools!(builder.tools(tool_vec), mcp_tools);

                Ok((AgentClient::Anthropic(agent), shell_session_out))
            }
            ProviderType::OpenAI => {
                let key =
                    api_key.ok_or_else(|| anyhow!("API key not configured for OpenAI provider"))?;

                let client = rig::providers::openai::Client::new(&key)?;
                let mut builder = client
                    .agent(&model_config.model_identifier)
                    .preamble(&preamble);

                // Detect reasoning models by identifier:
                // - o-series: o1, o3, o4, o1-mini, o3-mini, o4-mini, etc.
                // - gpt-5 series: gpt-5, gpt-5-mini, gpt-5-nano, etc.
                // Note: supports_temperature defaults to true, so we cannot rely on it alone
                // to detect reasoning models — users may not have explicitly set it.
                let is_reasoning_model = {
                    let id = model_config.model_identifier.as_str();
                    let is_o_series = {
                        let mut chars = id.chars();
                        chars.next() == Some('o')
                            && chars.next().is_some_and(|c| c.is_ascii_digit())
                    };
                    is_o_series || id.starts_with("gpt-5")
                };

                // Only set temperature if the model supports it (reasoning models don't)
                if model_config.supports_temperature && !is_reasoning_model {
                    builder = builder.temperature(model_config.temperature as f64);
                }

                if is_reasoning_model || !model_config.supports_temperature {
                    // TODO(#127): Remove once rig-core handles reasoning IDs correctly.
                    // Reasoning models (o-series, gpt-5) need explicit reasoning summary
                    // configuration for multi-turn tool calling to work. Without this, the
                    // Responses API doesn't include reasoning summaries in OutputItemDone
                    // events, causing rig-core to lose the reasoning ID when assembling
                    // conversation history. OpenAI then rejects the next turn with:
                    // "function_call was provided without its required reasoning item"
                    builder = builder.additional_params(serde_json::json!({
                        "reasoning": {
                            "summary": "auto"
                        }
                    }));
                }

                // Build with all tools (sanitize MCP schemas for OpenAI strict mode)
                let tool_vec = collect_tools(
                    list_tools,
                    fs_read_tools,
                    fs_write_tools,
                    add_attachment_tool.clone(),
                    pdf_to_image_tool.clone(),
                    pdf_info_tool.clone(),
                    pdf_extract_text_tool.clone(),
                    mcp_mgmt_tools,
                    fetch_tool.clone(),
                    shell_tools,
                    git_tools,
                    search_tools,
                    excel_read_tool.clone(),
                    excel_write_tools.clone(),
                    data_query_tools.clone(),
                    chart_tool.clone(),
                    typst_tool.clone(),
                    execute_code_tool.clone(),
                );
                let mcp_tools = sanitize_mcp_tools_for_openai(mcp_tools);
                let agent = build_with_mcp_tools!(builder.tools(tool_vec), mcp_tools);

                Ok((AgentClient::OpenAI(agent), shell_session_out))
            }
            ProviderType::Gemini => {
                let key =
                    api_key.ok_or_else(|| anyhow!("API key not configured for Gemini provider"))?;

                let client = rig::providers::gemini::Client::new(&key)?;
                let builder = client
                    .agent(&model_config.model_identifier)
                    .preamble(&preamble)
                    .temperature(model_config.temperature as f64);

                // Build with all tools
                let tool_vec = collect_tools(
                    list_tools,
                    fs_read_tools,
                    fs_write_tools,
                    add_attachment_tool.clone(),
                    pdf_to_image_tool.clone(),
                    pdf_info_tool.clone(),
                    pdf_extract_text_tool.clone(),
                    mcp_mgmt_tools,
                    fetch_tool.clone(),
                    shell_tools,
                    git_tools,
                    search_tools,
                    excel_read_tool.clone(),
                    excel_write_tools.clone(),
                    data_query_tools.clone(),
                    chart_tool.clone(),
                    typst_tool.clone(),
                    execute_code_tool.clone(),
                );
                let agent = build_with_mcp_tools!(builder.tools(tool_vec), mcp_tools);

                Ok((AgentClient::Gemini(agent), shell_session_out))
            }
            ProviderType::Mistral => {
                let key = api_key
                    .ok_or_else(|| anyhow!("API key not configured for Mistral provider"))?;

                let client = rig::providers::mistral::Client::new(&key)?;
                let mut builder = client
                    .agent(&model_config.model_identifier)
                    .preamble(&preamble)
                    .temperature(model_config.temperature as f64);

                if let Some(max_tokens) = model_config.max_tokens {
                    builder = builder.max_tokens(max_tokens as u64);
                }

                // Build with all tools
                let tool_vec = collect_tools(
                    list_tools,
                    fs_read_tools,
                    fs_write_tools,
                    add_attachment_tool.clone(),
                    pdf_to_image_tool.clone(),
                    pdf_info_tool.clone(),
                    pdf_extract_text_tool.clone(),
                    mcp_mgmt_tools,
                    fetch_tool.clone(),
                    shell_tools,
                    git_tools,
                    search_tools,
                    excel_read_tool.clone(),
                    excel_write_tools.clone(),
                    data_query_tools.clone(),
                    chart_tool.clone(),
                    typst_tool.clone(),
                    execute_code_tool.clone(),
                );
                let agent = build_with_mcp_tools!(builder.tools(tool_vec), mcp_tools);

                Ok((AgentClient::Mistral(agent), shell_session_out))
            }
            ProviderType::Ollama => {
                let url = base_url.unwrap_or_else(|| "http://localhost:11434".to_string());

                let client = rig::providers::ollama::Client::builder()
                    .api_key(rig::client::Nothing)
                    .base_url(&url)
                    .build()?;

                let builder = client
                    .agent(&model_config.model_identifier)
                    .preamble(&preamble)
                    .temperature(model_config.temperature as f64);

                // Build with all tools
                let tool_vec = collect_tools(
                    list_tools,
                    fs_read_tools,
                    fs_write_tools,
                    add_attachment_tool.clone(),
                    pdf_to_image_tool.clone(),
                    pdf_info_tool.clone(),
                    pdf_extract_text_tool.clone(),
                    mcp_mgmt_tools,
                    fetch_tool.clone(),
                    shell_tools,
                    git_tools,
                    search_tools,
                    excel_read_tool.clone(),
                    excel_write_tools.clone(),
                    data_query_tools.clone(),
                    chart_tool.clone(),
                    typst_tool.clone(),
                    execute_code_tool.clone(),
                );
                let agent = build_with_mcp_tools!(builder.tools(tool_vec), mcp_tools);

                Ok((AgentClient::Ollama(agent), shell_session_out))
            }
            ProviderType::AzureOpenAI => {
                let raw_endpoint = base_url.ok_or_else(|| {
                    anyhow!("Endpoint URL not configured for Azure OpenAI provider")
                })?;

                // Normalize the endpoint URL:
                // 1. Strip trailing slashes
                // 2. Add https:// if missing
                // 3. Extract base URL if user provided full path (e.g., .../openai/deployments/...)
                let raw_endpoint = raw_endpoint.trim_end_matches('/').to_string();
                let mut endpoint = if raw_endpoint.starts_with("http://")
                    || raw_endpoint.starts_with("https://")
                {
                    raw_endpoint
                } else {
                    format!("https://{}", raw_endpoint)
                };

                // If the endpoint contains /openai/deployments or /openai as a PATH component,
                // extract just the base URL. We need to be careful not to match "openai" in the hostname
                // (e.g., https://myresource.openai.azure.com should NOT be truncated)
                // Azure endpoint should be just https://myresource.openai.azure.com
                // rig-core appends /openai/deployments/{model}/chat/completions itself

                // Find the position after the scheme and hostname (after the third /)
                let hostname_end = endpoint.find("://").and_then(|scheme_pos| {
                    endpoint[scheme_pos + 3..]
                        .find('/')
                        .map(|p| scheme_pos + 3 + p)
                });

                if let Some(path_start) = hostname_end {
                    // Only look for /openai in the path portion, not the hostname
                    if let Some(pos) = endpoint[path_start..].find("/openai") {
                        endpoint.truncate(path_start + pos);
                    }
                }

                let api_version = model_config
                    .extra_params
                    .get("api_version")
                    .map(|s| s.as_str())
                    .unwrap_or(AZURE_DEFAULT_API_VERSION);

                // Validate the endpoint is a valid absolute URL
                if !endpoint.starts_with("http://") && !endpoint.starts_with("https://") {
                    return Err(anyhow!(
                        "Invalid Azure endpoint URL (must start with https://): '{}'",
                        endpoint
                    ));
                }

                // NEW: Determine auth method and build credentials
                let auth = match provider_config.azure_auth_method() {
                    AzureAuthMethod::EntraId => {
                        tracing::info!("Using Entra ID authentication with token cache");

                        // Get or create global token cache
                        // Note: OnceLock ensures initialization happens exactly once
                        let cache = AZURE_TOKEN_CACHE.get_or_init(|| {
                            match AzureTokenCache::new() {
                                Ok(cache) => Some(cache),
                                Err(e) => {
                                    tracing::warn!(
                                        error = ?e,
                                        "Failed to create Azure token cache, will fetch tokens directly each time"
                                    );
                                    None
                                }
                            }
                        });

                        let token = if let Some(cache) = cache {
                            cache
                                .get_token()
                                .await
                                .context("Failed to get cached Entra ID token")?
                        } else {
                            // Fallback to direct fetch if cache creation failed
                            // This happens on every agent creation - not ideal but functional
                            tracing::debug!("Using direct token fetch (cache unavailable)");
                            azure_auth::fetch_entra_id_token()
                                .await
                                .context("Failed to fetch Entra ID token")?
                        };

                        rig::providers::azure::AzureOpenAIAuth::Token(token)
                    }
                    AzureAuthMethod::ApiKey => {
                        tracing::info!("Using API Key authentication for Azure OpenAI");
                        let key = api_key.ok_or_else(|| {
                            anyhow!("API key not configured for Azure OpenAI provider")
                        })?;
                        rig::providers::azure::AzureOpenAIAuth::ApiKey(key)
                    }
                };

                // Log the normalized endpoint for debugging
                tracing::info!(
                    endpoint = %endpoint,
                    deployment = %model_config.model_identifier,
                    api_version = %api_version,
                    auth_method = ?provider_config.azure_auth_method(),
                    "Building Azure OpenAI client"
                );

                let client = rig::providers::azure::Client::builder()
                    .api_key(auth)
                    .azure_endpoint(endpoint.clone())
                    .api_version(api_version)
                    .build()
                    .map_err(|e| {
                        anyhow!(
                            "Failed to build Azure client with endpoint '{}': {}",
                            endpoint,
                            e
                        )
                    })?;

                let mut builder = client
                    .agent(&model_config.model_identifier)
                    .preamble(&preamble);

                let is_reasoning_model = {
                    let id = model_config.model_identifier.as_str();
                    let is_o_series = {
                        let mut chars = id.chars();
                        chars.next() == Some('o')
                            && chars.next().is_some_and(|c| c.is_ascii_digit())
                    };
                    is_o_series || id.starts_with("gpt-5")
                };

                if model_config.supports_temperature && !is_reasoning_model {
                    builder = builder.temperature(model_config.temperature as f64);
                }

                if is_reasoning_model || !model_config.supports_temperature {
                    builder = builder.additional_params(serde_json::json!({
                        "reasoning": {
                            "summary": "auto"
                        }
                    }));
                }

                if let Some(max_tokens) = model_config.max_tokens {
                    builder = builder.max_tokens(max_tokens as u64);
                }

                // Build with all tools (sanitize MCP schemas for OpenAI strict mode)
                let tool_vec = collect_tools(
                    list_tools,
                    fs_read_tools,
                    fs_write_tools,
                    add_attachment_tool.clone(),
                    pdf_to_image_tool.clone(),
                    pdf_info_tool.clone(),
                    pdf_extract_text_tool.clone(),
                    mcp_mgmt_tools,
                    fetch_tool.clone(),
                    shell_tools,
                    git_tools,
                    search_tools,
                    excel_read_tool.clone(),
                    excel_write_tools.clone(),
                    data_query_tools.clone(),
                    chart_tool.clone(),
                    typst_tool.clone(),
                    execute_code_tool.clone(),
                );
                let mcp_tools = sanitize_mcp_tools_for_openai(mcp_tools);
                let agent = build_with_mcp_tools!(builder.tools(tool_vec), mcp_tools);

                Ok((AgentClient::AzureOpenAI(agent), shell_session_out))
            }
        }
    }

    /// Returns the provider name for logging/debugging.
    #[allow(dead_code)]
    pub fn provider_name(&self) -> &'static str {
        match self {
            AgentClient::Anthropic(_) => "Anthropic",
            AgentClient::OpenAI(_) => "OpenAI",
            AgentClient::Gemini(_) => "Gemini",
            AgentClient::Ollama(_) => "Ollama",
            AgentClient::Mistral(_) => "Mistral",
            AgentClient::AzureOpenAI(_) => "Azure OpenAI",
        }
    }
}

#[cfg(test)]
mod tests {

    /// Helper function to normalize Azure endpoint URL (extracted from agent creation logic)
    fn normalize_azure_endpoint(raw_endpoint: &str) -> String {
        // 1. Strip trailing slashes
        let raw_endpoint = raw_endpoint.trim_end_matches('/').to_string();

        // 2. Add https:// if missing
        let mut endpoint =
            if raw_endpoint.starts_with("http://") || raw_endpoint.starts_with("https://") {
                raw_endpoint
            } else {
                format!("https://{}", raw_endpoint)
            };

        // 3. Extract base URL if user provided full path (e.g., .../openai/deployments/...)
        // Find the position after the scheme and hostname (after the third /)
        let hostname_end = endpoint.find("://").and_then(|scheme_pos| {
            endpoint[scheme_pos + 3..]
                .find('/')
                .map(|p| scheme_pos + 3 + p)
        });

        if let Some(path_start) = hostname_end {
            // Only look for /openai in the path portion, not the hostname
            if let Some(pos) = endpoint[path_start..].find("/openai") {
                endpoint.truncate(path_start + pos);
            }
        }

        endpoint
    }

    #[test]
    fn test_azure_url_normalization_basic() {
        // Simple hostname without scheme
        assert_eq!(
            normalize_azure_endpoint("myresource.openai.azure.com"),
            "https://myresource.openai.azure.com"
        );
    }

    #[test]
    fn test_azure_url_normalization_with_https() {
        // Already has https scheme
        assert_eq!(
            normalize_azure_endpoint("https://myresource.openai.azure.com"),
            "https://myresource.openai.azure.com"
        );
    }

    #[test]
    fn test_azure_url_normalization_with_http() {
        // Has http scheme (should be preserved)
        assert_eq!(
            normalize_azure_endpoint("http://myresource.openai.azure.com"),
            "http://myresource.openai.azure.com"
        );
    }

    #[test]
    fn test_azure_url_normalization_trailing_slash() {
        // Trailing slash should be removed
        assert_eq!(
            normalize_azure_endpoint("myresource.openai.azure.com/"),
            "https://myresource.openai.azure.com"
        );
    }

    #[test]
    fn test_azure_url_normalization_multiple_trailing_slashes() {
        // Multiple trailing slashes should be removed
        assert_eq!(
            normalize_azure_endpoint("https://myresource.openai.azure.com///"),
            "https://myresource.openai.azure.com"
        );
    }

    #[test]
    fn test_azure_url_normalization_with_openai_path() {
        // Full path with /openai should be truncated to base URL
        assert_eq!(
            normalize_azure_endpoint("https://my.openai.azure.com/openai/deployments/gpt4"),
            "https://my.openai.azure.com"
        );
    }

    #[test]
    fn test_azure_url_normalization_with_openai_deployments_path() {
        // Path with /openai/deployments should be truncated
        assert_eq!(
            normalize_azure_endpoint("https://test.openai.azure.com/openai/deployments/"),
            "https://test.openai.azure.com"
        );
    }

    #[test]
    fn test_azure_url_normalization_openai_in_hostname() {
        // "openai" in hostname should NOT be truncated
        assert_eq!(
            normalize_azure_endpoint("https://myresource.openai.azure.com"),
            "https://myresource.openai.azure.com"
        );
    }

    #[test]
    fn test_azure_url_normalization_openai_in_subdomain() {
        // "openai" in subdomain should NOT be truncated
        assert_eq!(
            normalize_azure_endpoint("https://openai.example.com"),
            "https://openai.example.com"
        );
    }

    #[test]
    fn test_azure_url_normalization_complex_path() {
        // Complex path with /openai should be truncated
        assert_eq!(
            normalize_azure_endpoint(
                "myresource.openai.azure.com/openai/deployments/model/chat/completions"
            ),
            "https://myresource.openai.azure.com"
        );
    }

    #[test]
    fn test_azure_url_normalization_path_without_openai() {
        // Path without /openai should be preserved (edge case)
        assert_eq!(
            normalize_azure_endpoint("https://myresource.azure.com/api/v1"),
            "https://myresource.azure.com/api/v1"
        );
    }

    #[test]
    fn test_azure_url_normalization_custom_port() {
        // Custom port should be preserved
        assert_eq!(
            normalize_azure_endpoint("https://localhost:8080/openai/deployments"),
            "https://localhost:8080"
        );
    }

    #[test]
    fn test_azure_url_normalization_no_scheme_with_path() {
        // No scheme with path containing /openai
        assert_eq!(
            normalize_azure_endpoint("myresource.openai.azure.com/openai"),
            "https://myresource.openai.azure.com"
        );
    }
}
