use anyhow::{Context, Result, anyhow};
use rig::agent::Agent;
use rig::client::CompletionClient;
use std::sync::OnceLock;

use crate::chatty::auth::{AzureTokenCache, azure_auth};
use crate::chatty::services::filesystem_service::FileSystemService;
use crate::chatty::tools::{
    ApplyDiffTool, CreateDirectoryTool, DeleteFileTool, GlobSearchTool, ListDirectoryTool,
    ListToolsTool, MoveFileTool, ReadBinaryTool, ReadFileTool, WriteFileTool,
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

macro_rules! build_with_mcp_tools {
    ($builder:expr, $mcp_tools:expr) => {{
        match $mcp_tools {
            Some(tools_list) => {
                let mut iter = tools_list.into_iter().filter(|(t, _)| !t.is_empty());
                if let Some((first_tools, first_sink)) = iter.next() {
                    let mut b = $builder.rmcp_tools(first_tools, first_sink);
                    for (tools, sink) in iter {
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

/// Build an agent with optional bash, filesystem read, and filesystem write tools,
/// then optional MCP tools. The list_tools tool is always included.
///
/// Due to rig's type-level tool chaining, each combination of tool presence/absence
/// produces a different builder type. This macro enumerates all 8 combinations
/// (bash × fs_read × fs_write) explicitly.
macro_rules! build_agent_with_tools {
    ($builder:expr, $bash_tool:expr, $fs_read:expr, $fs_write:expr, $list_tools:expr, $mcp_tools:expr) => {{
        match (&$bash_tool, &$fs_read, &$fs_write) {
            (Some(bash), Some((rf, rb, ld, gs)), Some((wf, cd, df, mf, ad))) => {
                let b = $builder
                    .tool($list_tools.clone())
                    .tool(bash.clone())
                    .tool(rf.clone())
                    .tool(rb.clone())
                    .tool(ld.clone())
                    .tool(gs.clone())
                    .tool(wf.clone())
                    .tool(cd.clone())
                    .tool(df.clone())
                    .tool(mf.clone())
                    .tool(ad.clone());
                build_with_mcp_tools!(b, $mcp_tools)
            }
            (Some(bash), Some((rf, rb, ld, gs)), None) => {
                let b = $builder
                    .tool($list_tools.clone())
                    .tool(bash.clone())
                    .tool(rf.clone())
                    .tool(rb.clone())
                    .tool(ld.clone())
                    .tool(gs.clone());
                build_with_mcp_tools!(b, $mcp_tools)
            }
            (Some(bash), None, Some((wf, cd, df, mf, ad))) => {
                let b = $builder
                    .tool($list_tools.clone())
                    .tool(bash.clone())
                    .tool(wf.clone())
                    .tool(cd.clone())
                    .tool(df.clone())
                    .tool(mf.clone())
                    .tool(ad.clone());
                build_with_mcp_tools!(b, $mcp_tools)
            }
            (Some(bash), None, None) => {
                let b = $builder.tool($list_tools.clone()).tool(bash.clone());
                build_with_mcp_tools!(b, $mcp_tools)
            }
            (None, Some((rf, rb, ld, gs)), Some((wf, cd, df, mf, ad))) => {
                let b = $builder
                    .tool($list_tools.clone())
                    .tool(rf.clone())
                    .tool(rb.clone())
                    .tool(ld.clone())
                    .tool(gs.clone())
                    .tool(wf.clone())
                    .tool(cd.clone())
                    .tool(df.clone())
                    .tool(mf.clone())
                    .tool(ad.clone());
                build_with_mcp_tools!(b, $mcp_tools)
            }
            (None, Some((rf, rb, ld, gs)), None) => {
                let b = $builder
                    .tool($list_tools.clone())
                    .tool(rf.clone())
                    .tool(rb.clone())
                    .tool(ld.clone())
                    .tool(gs.clone());
                build_with_mcp_tools!(b, $mcp_tools)
            }
            (None, None, Some((wf, cd, df, mf, ad))) => {
                let b = $builder
                    .tool($list_tools.clone())
                    .tool(wf.clone())
                    .tool(cd.clone())
                    .tool(df.clone())
                    .tool(mf.clone())
                    .tool(ad.clone());
                build_with_mcp_tools!(b, $mcp_tools)
            }
            (None, None, None) => {
                let b = $builder.tool($list_tools.clone());
                build_with_mcp_tools!(b, $mcp_tools)
            }
        }
    }};
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
    pub async fn from_model_config_with_tools(
        model_config: &ModelConfig,
        provider_config: &ProviderConfig,
        mcp_tools: Option<Vec<(Vec<rmcp::model::Tool>, rmcp::service::ServerSink)>>,
        exec_settings: Option<crate::settings::models::ExecutionSettingsModel>,
        pending_approvals: Option<
            crate::chatty::models::execution_approval_store::PendingApprovals,
        >,
        pending_write_approvals: Option<
            crate::chatty::models::write_approval_store::PendingWriteApprovals,
        >,
    ) -> Result<Self> {
        let api_key = provider_config.api_key.clone();
        let base_url = provider_config.base_url.clone();

        // Create BashTool if execution is enabled
        let bash_tool = if let (Some(settings), Some(approvals)) =
            (&exec_settings, &pending_approvals)
        {
            if settings.enabled {
                Some(crate::chatty::tools::BashTool::new(std::sync::Arc::new(
                    crate::chatty::tools::BashExecutor::new(settings.clone(), approvals.clone()),
                )))
            } else {
                None
            }
        } else {
            None
        };

        // Create filesystem tools if a workspace directory is configured
        let (fs_read_tools, fs_write_tools): (Option<FsReadTools>, Option<FsWriteTools>) =
            match exec_settings
                .as_ref()
                .and_then(|s| s.workspace_dir.as_ref())
            {
                Some(workspace_dir) => match FileSystemService::new(workspace_dir) {
                    Ok(service) => {
                        let service = std::sync::Arc::new(service);
                        tracing::info!(workspace = %workspace_dir, "Filesystem tools enabled");

                        let read_tools = (
                            ReadFileTool::new(service.clone()),
                            ReadBinaryTool::new(service.clone()),
                            ListDirectoryTool::new(service.clone()),
                            GlobSearchTool::new(service.clone()),
                        );

                        // Write tools also need pending_write_approvals
                        let write_tools = pending_write_approvals.as_ref().map(|approvals| {
                            (
                                WriteFileTool::new(service.clone(), approvals.clone()),
                                CreateDirectoryTool::new(service.clone()),
                                DeleteFileTool::new(service.clone(), approvals.clone()),
                                MoveFileTool::new(service.clone(), approvals.clone()),
                                ApplyDiffTool::new(service.clone(), approvals.clone()),
                            )
                        });

                        (Some(read_tools), write_tools)
                    }
                    Err(e) => {
                        tracing::warn!(error = ?e, workspace = %workspace_dir, "Failed to initialize filesystem tools");
                        (None, None)
                    }
                },
                None => (None, None),
            };

        // Create list_tools tool (always available, but shows only actually available tools)
        let list_tools = ListToolsTool::new_with_config(
            bash_tool.is_some(),
            fs_read_tools.is_some(),
            fs_write_tools.is_some(),
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
                let agent = build_agent_with_tools!(
                    builder,
                    bash_tool,
                    fs_read_tools,
                    fs_write_tools,
                    list_tools,
                    mcp_tools
                );

                Ok(AgentClient::Anthropic(agent))
            }
            ProviderType::OpenAI => {
                let key =
                    api_key.ok_or_else(|| anyhow!("API key not configured for OpenAI provider"))?;

                let client = rig::providers::openai::Client::new(&key)?;
                let mut builder = client
                    .agent(&model_config.model_identifier)
                    .preamble(&model_config.preamble);

                // Only set temperature if the model supports it
                if model_config.supports_temperature {
                    builder = builder.temperature(model_config.temperature as f64);
                }

                // Build with all tools
                let agent = build_agent_with_tools!(
                    builder,
                    bash_tool,
                    fs_read_tools,
                    fs_write_tools,
                    list_tools,
                    mcp_tools
                );

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
                let agent = build_agent_with_tools!(
                    builder,
                    bash_tool,
                    fs_read_tools,
                    fs_write_tools,
                    list_tools,
                    mcp_tools
                );

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
                let agent = build_agent_with_tools!(
                    builder,
                    bash_tool,
                    fs_read_tools,
                    fs_write_tools,
                    list_tools,
                    mcp_tools
                );

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
                let agent = build_agent_with_tools!(
                    builder,
                    bash_tool,
                    fs_read_tools,
                    fs_write_tools,
                    list_tools,
                    mcp_tools
                );

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

                // Build with all tools
                let agent = build_agent_with_tools!(
                    builder,
                    bash_tool,
                    fs_read_tools,
                    fs_write_tools,
                    list_tools,
                    mcp_tools
                );

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
