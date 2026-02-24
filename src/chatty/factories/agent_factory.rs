use anyhow::{Context, Result, anyhow};
use rig::agent::Agent;
use rig::client::CompletionClient;
use rig::tool::ToolDyn;
use std::sync::OnceLock;

use crate::chatty::auth::{AzureTokenCache, azure_auth};
use crate::chatty::services::filesystem_service::FileSystemService;
use crate::chatty::services::git_service::GitService;
use crate::chatty::services::search_service::CodeSearchService;
use crate::chatty::services::shell_service::ShellSession;
use crate::chatty::tools::{
    AddAttachmentTool, AddMcpTool, ApplyDiffTool, CreateDirectoryTool, DeleteFileTool,
    DeleteMcpTool, EditMcpTool, FetchTool, FindDefinitionTool, FindFilesTool, GitAddTool,
    GitCommitTool, GitCreateBranchTool, GitDiffTool, GitLogTool, GitStatusTool,
    GitSwitchBranchTool, GlobSearchTool, ListDirectoryTool, ListMcpTool, ListToolsTool,
    MoveFileTool, PendingArtifacts, ReadBinaryTool, ReadFileTool, SearchCodeTool, ShellCdTool,
    ShellExecuteTool, ShellSetEnvTool, ShellStatusTool, WriteFileTool,
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
    mcp_mgmt: McpTools,
    fetch_tool: Option<FetchTool>,
    shell_tools: Option<ShellTools>,
    git_tools: Option<GitTools>,
    search_tools: Option<SearchTools>,
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
            crate::chatty::models::execution_approval_store::PendingApprovals,
        >,
        pending_write_approvals: Option<
            crate::chatty::models::write_approval_store::PendingWriteApprovals,
        >,
        pending_artifacts: Option<PendingArtifacts>,
        shell_session: Option<std::sync::Arc<ShellSession>>,
    ) -> Result<(Self, Option<std::sync::Arc<ShellSession>>)> {
        let api_key = provider_config.api_key.clone();
        let base_url = provider_config.base_url.clone();

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
                    Some(std::sync::Arc::new(ShellSession::new(
                        settings.workspace_dir.clone(),
                        settings.timeout_seconds,
                        settings.max_output_bytes,
                        settings.network_isolation,
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
        let mut search_tools: Option<SearchTools> = None;
        let (fs_read_tools, fs_write_tools): (Option<FsReadTools>, Option<FsWriteTools>) =
            match exec_settings
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

                            // Create add_attachment tool alongside read tools
                            // Only register if the model supports at least one multimodal type
                            if let Some(ref artifacts) = pending_artifacts {
                                let supports_images = model_config.supports_images;
                                let supports_pdf = model_config.supports_pdf;
                                if supports_images || supports_pdf {
                                    add_attachment_tool = Some(AddAttachmentTool::new(
                                        service.clone(),
                                        artifacts.clone(),
                                        supports_images,
                                        supports_pdf,
                                    ));
                                }
                            }

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

                        (read_tools, write_tools)
                    }
                    Err(e) => {
                        tracing::warn!(error = ?e, workspace = %workspace_dir, "Failed to initialize filesystem tools");
                        (None, None)
                    }
                },
                None => (None, None),
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

        // Create list_tools tool (always available, shows native + MCP tools)
        let list_tools = ListToolsTool::new_with_config(
            fs_read_tools.is_some(),
            fs_write_tools.is_some(),
            mcp_mgmt_tools.is_enabled(),
            fetch_tool.is_some(),
            shell_tools.is_some(),
            git_tools.is_some(),
            search_tools.is_some(),
            mcp_tool_info,
        );

        match &provider_config.provider_type {
            ProviderType::Anthropic => {
                let key = api_key
                    .ok_or_else(|| anyhow!("API key not configured for Anthropic provider"))?;

                let client = rig::providers::anthropic::Client::new(&key)?;
                let mut builder = client
                    .agent(&model_config.model_identifier)
                    .preamble(&model_config.preamble)
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
                    mcp_mgmt_tools,
                    fetch_tool.clone(),
                    shell_tools,
                    git_tools,
                    search_tools,
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
                    .preamble(&model_config.preamble);

                // Only set temperature if the model supports it (reasoning models don't)
                if model_config.supports_temperature {
                    builder = builder.temperature(model_config.temperature as f64);
                } else {
                    // TODO(#127): Remove once rig-core handles reasoning IDs correctly.
                    // Reasoning models (o-series, gpt-5-nano, etc.) need explicit reasoning
                    // summary configuration for multi-turn tool calling to work. Without this,
                    // the Responses API doesn't include reasoning summaries in OutputItemDone
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
                    mcp_mgmt_tools,
                    fetch_tool.clone(),
                    shell_tools,
                    git_tools,
                    search_tools,
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
                    .preamble(&model_config.preamble)
                    .temperature(model_config.temperature as f64);

                // Build with all tools
                let tool_vec = collect_tools(
                    list_tools,
                    fs_read_tools,
                    fs_write_tools,
                    add_attachment_tool.clone(),
                    mcp_mgmt_tools,
                    fetch_tool.clone(),
                    shell_tools,
                    git_tools,
                    search_tools,
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
                    .preamble(&model_config.preamble)
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
                    mcp_mgmt_tools,
                    fetch_tool.clone(),
                    shell_tools,
                    git_tools,
                    search_tools,
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
                    .preamble(&model_config.preamble)
                    .temperature(model_config.temperature as f64);

                // Build with all tools
                let tool_vec = collect_tools(
                    list_tools,
                    fs_read_tools,
                    fs_write_tools,
                    add_attachment_tool.clone(),
                    mcp_mgmt_tools,
                    fetch_tool.clone(),
                    shell_tools,
                    git_tools,
                    search_tools,
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
                    .preamble(&model_config.preamble);

                if model_config.supports_temperature {
                    builder = builder.temperature(model_config.temperature as f64);
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
                    mcp_mgmt_tools,
                    fetch_tool.clone(),
                    shell_tools,
                    git_tools,
                    search_tools,
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
