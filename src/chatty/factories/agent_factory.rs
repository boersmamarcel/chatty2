use anyhow::{Context, Result, anyhow};
use rig::agent::Agent;
use rig::client::CompletionClient;
use std::sync::OnceLock;

use crate::chatty::auth::{AzureTokenCache, azure_auth};
use crate::settings::models::models_store::{AZURE_DEFAULT_API_VERSION, ModelConfig};
use crate::settings::models::providers_store::{AzureAuthMethod, ProviderConfig, ProviderType};

static AZURE_TOKEN_CACHE: OnceLock<Option<AzureTokenCache>> = OnceLock::new();

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
    /// Create AgentClient from ModelConfig and ProviderConfig with optional MCP and native tools
    pub async fn from_model_config_with_tools(
        model_config: &ModelConfig,
        provider_config: &ProviderConfig,
        mcp_tools: Option<Vec<(Vec<rmcp::model::Tool>, rmcp::service::ServerSink)>>,
        native_tools: Option<Vec<std::sync::Arc<dyn rig::tool::Tool<Error = anyhow::Error> + Send + Sync>>>,
    ) -> Result<Self> {
        let api_key = provider_config.api_key.clone();
        let base_url = provider_config.base_url.clone();

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

                // Add native tools if provided
                if let Some(tools) = native_tools {
                    for tool in tools {
                        builder = builder.tool(tool);
                    }
                }

                let agent = build_with_mcp_tools!(builder, mcp_tools);

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

                // Add native tools if provided
                if let Some(tools) = native_tools {
                    for tool in tools {
                        builder = builder.tool(tool);
                    }
                }

                let agent = build_with_mcp_tools!(builder, mcp_tools);

                Ok(AgentClient::OpenAI(agent))
            }
            ProviderType::Gemini => {
                let key =
                    api_key.ok_or_else(|| anyhow!("API key not configured for Gemini provider"))?;

                let client = rig::providers::gemini::Client::new(&key)?;
                let mut builder = client
                    .agent(&model_config.model_identifier)
                    .preamble(&model_config.preamble)
                    .temperature(model_config.temperature as f64);

                // Add native tools if provided
                if let Some(tools) = native_tools {
                    for tool in tools {
                        builder = builder.tool(tool);
                    }
                }

                let agent = build_with_mcp_tools!(builder, mcp_tools);

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

                // Add native tools if provided
                if let Some(tools) = native_tools {
                    for tool in tools {
                        builder = builder.tool(tool);
                    }
                }

                let agent = build_with_mcp_tools!(builder, mcp_tools);

                Ok(AgentClient::Mistral(agent))
            }
            ProviderType::Ollama => {
                let url = base_url.unwrap_or_else(|| "http://localhost:11434".to_string());

                let client = rig::providers::ollama::Client::builder()
                    .api_key(rig::client::Nothing)
                    .base_url(&url)
                    .build()?;

                let mut builder = client
                    .agent(&model_config.model_identifier)
                    .preamble(&model_config.preamble)
                    .temperature(model_config.temperature as f64);

                // Add native tools if provided
                if let Some(tools) = native_tools {
                    for tool in tools {
                        builder = builder.tool(tool);
                    }
                }

                let agent = build_with_mcp_tools!(builder, mcp_tools);

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

                // Add native tools if provided
                if let Some(tools) = native_tools {
                    for tool in tools {
                        builder = builder.tool(tool);
                    }
                }

                let agent = build_with_mcp_tools!(builder, mcp_tools);

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
