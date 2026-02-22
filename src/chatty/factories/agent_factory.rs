use anyhow::{Context, Result, anyhow};
use rig::agent::Agent;
use rig::client::CompletionClient;
use rig::tool::ToolDyn;
use std::sync::OnceLock;

use crate::chatty::auth::{AzureTokenCache, azure_auth};
use crate::chatty::services::filesystem_service::FileSystemService;
use crate::chatty::tools::{
    AddAttachmentTool, AddMcpTool, ApplyDiffTool, BashTool, CreateDirectoryTool, DeleteFileTool,
    DeleteMcpTool, EditMcpTool, GlobSearchTool, ListDirectoryTool, ListMcpTool, ListToolsTool,
    MoveFileTool, PendingArtifacts, ReadBinaryTool, ReadFileTool, WriteFileTool,
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

macro_rules! build_with_mcp_tools {
    ($builder:expr, $mcp_tools:expr) => {{
        match $mcp_tools {
            Some(tools_list) => {
                let mut iter = tools_list
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
fn collect_tools(
    list_tools: ListToolsTool,
    bash_tool: Option<BashTool>,
    fs_read: Option<FsReadTools>,
    fs_write: Option<FsWriteTools>,
    add_attachment: Option<AddAttachmentTool>,
    mcp_mgmt: McpTools,
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
    if let Some(t) = bash_tool {
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
    /// Create AgentClient from ModelConfig and ProviderConfig with optional MCP tools, bash execution, and filesystem tools
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
    ) -> Result<Self> {
        let api_key = provider_config.api_key.clone();
        let base_url = provider_config.base_url.clone();

        // Create BashTool if execution is enabled
        let bash_tool = if let (Some(settings), Some(approvals)) =
            (&exec_settings, &pending_approvals)
        {
            if settings.enabled {
                let executor =
                    crate::chatty::tools::BashExecutor::new(settings.clone(), approvals.clone());
                Some(crate::chatty::tools::BashTool::new(std::sync::Arc::new(
                    executor,
                )))
            } else {
                None
            }
        } else {
            None
        };

        // Create filesystem tools if a workspace directory is configured
        let mut add_attachment_tool: Option<AddAttachmentTool> = None;
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

        // Create list_tools tool (always available, shows native + MCP tools)
        let list_tools = ListToolsTool::new_with_config(
            bash_tool.is_some(),
            fs_read_tools.is_some(),
            fs_write_tools.is_some(),
            mcp_mgmt_tools.is_enabled(),
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
                    bash_tool,
                    fs_read_tools,
                    fs_write_tools,
                    add_attachment_tool.clone(),
                    mcp_mgmt_tools,
                );
                let agent = build_with_mcp_tools!(builder.tools(tool_vec), mcp_tools);

                Ok(AgentClient::Anthropic(agent))
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
                    bash_tool,
                    fs_read_tools,
                    fs_write_tools,
                    add_attachment_tool.clone(),
                    mcp_mgmt_tools,
                );
                let mcp_tools = sanitize_mcp_tools_for_openai(mcp_tools);
                let agent = build_with_mcp_tools!(builder.tools(tool_vec), mcp_tools);

                Ok(AgentClient::OpenAI(agent))
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
                    bash_tool,
                    fs_read_tools,
                    fs_write_tools,
                    add_attachment_tool.clone(),
                    mcp_mgmt_tools,
                );
                let agent = build_with_mcp_tools!(builder.tools(tool_vec), mcp_tools);

                Ok(AgentClient::Gemini(agent))
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
                    bash_tool,
                    fs_read_tools,
                    fs_write_tools,
                    add_attachment_tool.clone(),
                    mcp_mgmt_tools,
                );
                let agent = build_with_mcp_tools!(builder.tools(tool_vec), mcp_tools);

                Ok(AgentClient::Mistral(agent))
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
                    bash_tool,
                    fs_read_tools,
                    fs_write_tools,
                    add_attachment_tool.clone(),
                    mcp_mgmt_tools,
                );
                let agent = build_with_mcp_tools!(builder.tools(tool_vec), mcp_tools);

                Ok(AgentClient::Ollama(agent))
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
                    bash_tool,
                    fs_read_tools,
                    fs_write_tools,
                    add_attachment_tool.clone(),
                    mcp_mgmt_tools,
                );
                let mcp_tools = sanitize_mcp_tools_for_openai(mcp_tools);
                let agent = build_with_mcp_tools!(builder.tools(tool_vec), mcp_tools);

                Ok(AgentClient::AzureOpenAI(agent))
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
