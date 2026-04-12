//! Provider-specific agent construction.
//!
//! Encapsulates the logic that differs between LLM providers: client creation,
//! builder configuration (temperature, reasoning hints, max tokens), and any
//! provider-specific schema sanitization (e.g. OpenAI `"format"` stripping).

use std::collections::HashSet;
use std::sync::OnceLock;

use anyhow::{Context, Result, anyhow};
use rig::client::CompletionClient;
use rig::tool::ToolDyn;

use crate::auth::{AzureTokenCache, azure_auth};
use crate::settings::models::models_store::{AZURE_DEFAULT_API_VERSION, ModelConfig};
use crate::settings::models::providers_store::{AzureAuthMethod, ProviderConfig, ProviderType};

use super::AgentClient;
use super::mcp_helpers::{build_with_mcp_tools, sanitize_mcp_tools_for_openai};

static AZURE_TOKEN_CACHE: OnceLock<Option<AzureTokenCache>> = OnceLock::new();

type McpToolSet = Vec<(String, Vec<rmcp::model::Tool>, rmcp::service::ServerSink)>;

/// Build a provider-specific `AgentClient` from pre-collected native tools.
///
/// All tool construction is done before this function — it only handles
/// provider client creation, builder configuration, and MCP attachment.
pub(super) async fn build_provider_agent(
    model_config: &ModelConfig,
    provider_config: &ProviderConfig,
    preamble: &str,
    tool_vec: Vec<Box<dyn ToolDyn>>,
    mcp_tools: Option<McpToolSet>,
    native_tool_names: &HashSet<String>,
) -> Result<AgentClient> {
    let api_key = provider_config.api_key.clone();
    let base_url = provider_config.base_url.clone();

    match &provider_config.provider_type {
        ProviderType::Anthropic => {
            let key =
                api_key.ok_or_else(|| anyhow!("API key not configured for Anthropic provider"))?;

            let client = rig::providers::anthropic::Client::new(&key)?;
            let mut builder = client
                .agent(&model_config.model_identifier)
                .preamble(preamble)
                .temperature(model_config.temperature as f64);

            if let Some(max_tokens) = model_config.max_tokens {
                builder = builder.max_tokens(max_tokens as u64);
            }

            let agent =
                build_with_mcp_tools!(builder.tools(tool_vec), mcp_tools, native_tool_names);
            Ok(AgentClient::Anthropic(agent))
        }
        ProviderType::OpenAI => {
            let key =
                api_key.ok_or_else(|| anyhow!("API key not configured for OpenAI provider"))?;

            let client = rig::providers::openai::Client::new(&key)?;
            let mut builder = client
                .agent(&model_config.model_identifier)
                .preamble(preamble);

            let is_reasoning = is_reasoning_model(&model_config.model_identifier);

            if model_config.supports_temperature && !is_reasoning {
                builder = builder.temperature(model_config.temperature as f64);
            }

            if is_reasoning || !model_config.supports_temperature {
                builder = builder.additional_params(serde_json::json!({
                    "reasoning": { "summary": "auto" }
                }));
            }

            let mcp_tools = sanitize_mcp_tools_for_openai(mcp_tools);
            let agent =
                build_with_mcp_tools!(builder.tools(tool_vec), mcp_tools, native_tool_names);
            Ok(AgentClient::OpenAI(agent))
        }
        ProviderType::Gemini => {
            let key =
                api_key.ok_or_else(|| anyhow!("API key not configured for Gemini provider"))?;

            let client = rig::providers::gemini::Client::new(&key)?;
            let builder = client
                .agent(&model_config.model_identifier)
                .preamble(preamble)
                .temperature(model_config.temperature as f64);

            let agent =
                build_with_mcp_tools!(builder.tools(tool_vec), mcp_tools, native_tool_names);
            Ok(AgentClient::Gemini(agent))
        }
        ProviderType::Mistral => {
            let key =
                api_key.ok_or_else(|| anyhow!("API key not configured for Mistral provider"))?;

            let client = rig::providers::mistral::Client::new(&key)?;
            let mut builder = client
                .agent(&model_config.model_identifier)
                .preamble(preamble)
                .temperature(model_config.temperature as f64);

            if let Some(max_tokens) = model_config.max_tokens {
                builder = builder.max_tokens(max_tokens as u64);
            }

            let agent =
                build_with_mcp_tools!(builder.tools(tool_vec), mcp_tools, native_tool_names);
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
                .preamble(preamble)
                .temperature(model_config.temperature as f64);

            let agent =
                build_with_mcp_tools!(builder.tools(tool_vec), mcp_tools, native_tool_names);
            Ok(AgentClient::Ollama(agent))
        }
        ProviderType::AzureOpenAI => {
            build_azure_agent(
                model_config,
                provider_config,
                preamble,
                tool_vec,
                mcp_tools,
                native_tool_names,
                api_key,
                base_url,
            )
            .await
        }
    }
}

/// Azure OpenAI has more complex setup (endpoint normalization, Entra ID auth),
/// so it gets its own function.
#[allow(clippy::too_many_arguments)]
async fn build_azure_agent(
    model_config: &ModelConfig,
    provider_config: &ProviderConfig,
    preamble: &str,
    tool_vec: Vec<Box<dyn ToolDyn>>,
    mcp_tools: Option<McpToolSet>,
    native_tool_names: &HashSet<String>,
    api_key: Option<String>,
    base_url: Option<String>,
) -> Result<AgentClient> {
    let raw_endpoint =
        base_url.ok_or_else(|| anyhow!("Endpoint URL not configured for Azure OpenAI provider"))?;

    let endpoint = normalize_azure_endpoint(&raw_endpoint);

    let api_version = model_config
        .extra_params
        .get("api_version")
        .map(|s| s.as_str())
        .unwrap_or(AZURE_DEFAULT_API_VERSION);

    if !endpoint.starts_with("http://") && !endpoint.starts_with("https://") {
        return Err(anyhow!(
            "Invalid Azure endpoint URL (must start with https://): '{}'",
            endpoint
        ));
    }

    let auth = match provider_config.azure_auth_method() {
        AzureAuthMethod::EntraId => {
            tracing::info!("Using Entra ID authentication with token cache");

            let cache = AZURE_TOKEN_CACHE.get_or_init(|| match AzureTokenCache::new() {
                Ok(cache) => Some(cache),
                Err(e) => {
                    tracing::warn!(
                        error = ?e,
                        "Failed to create Azure token cache, will fetch tokens directly each time"
                    );
                    None
                }
            });

            let token = if let Some(cache) = cache {
                cache
                    .get_token()
                    .await
                    .context("Failed to get cached Entra ID token")?
            } else {
                tracing::debug!("Using direct token fetch (cache unavailable)");
                azure_auth::fetch_entra_id_token()
                    .await
                    .context("Failed to fetch Entra ID token")?
            };

            rig::providers::azure::AzureOpenAIAuth::Token(token)
        }
        AzureAuthMethod::ApiKey => {
            tracing::info!("Using API Key authentication for Azure OpenAI");
            let key = api_key
                .ok_or_else(|| anyhow!("API key not configured for Azure OpenAI provider"))?;
            rig::providers::azure::AzureOpenAIAuth::ApiKey(key)
        }
    };

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
        .preamble(preamble);

    let is_reasoning = is_reasoning_model(&model_config.model_identifier);

    if model_config.supports_temperature && !is_reasoning {
        builder = builder.temperature(model_config.temperature as f64);
    }

    if is_reasoning || !model_config.supports_temperature {
        builder = builder.additional_params(serde_json::json!({
            "reasoning_effort": "medium"
        }));
    }

    if let Some(max_tokens) = model_config.max_tokens {
        builder = builder.max_tokens(max_tokens as u64);
    }

    let mcp_tools = sanitize_mcp_tools_for_openai(mcp_tools);
    let agent = build_with_mcp_tools!(builder.tools(tool_vec), mcp_tools, native_tool_names);
    Ok(AgentClient::AzureOpenAI(agent))
}

/// Detect models that use reasoning tokens (OpenAI o-series and GPT-5).
fn is_reasoning_model(model_identifier: &str) -> bool {
    let is_o_series = {
        let mut chars = model_identifier.chars();
        chars.next() == Some('o') && chars.next().is_some_and(|c| c.is_ascii_digit())
    };
    is_o_series || model_identifier.starts_with("gpt-5")
}

/// Normalize Azure endpoint URL:
/// 1. Strip trailing slashes
/// 2. Add https:// if missing
/// 3. Extract base URL if user provided full path (e.g., .../openai/deployments/...)
fn normalize_azure_endpoint(raw_endpoint: &str) -> String {
    let raw_endpoint = raw_endpoint.trim_end_matches('/').to_string();
    let mut endpoint =
        if raw_endpoint.starts_with("http://") || raw_endpoint.starts_with("https://") {
            raw_endpoint
        } else {
            format!("https://{}", raw_endpoint)
        };

    let hostname_end = endpoint.find("://").and_then(|scheme_pos| {
        endpoint[scheme_pos + 3..]
            .find('/')
            .map(|p| scheme_pos + 3 + p)
    });

    if let Some(path_start) = hostname_end
        && let Some(pos) = endpoint[path_start..].find("/openai")
    {
        endpoint.truncate(path_start + pos);
    }

    endpoint
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_azure_url_normalization_basic() {
        assert_eq!(
            normalize_azure_endpoint("myresource.openai.azure.com"),
            "https://myresource.openai.azure.com"
        );
    }

    #[test]
    fn test_azure_url_normalization_with_https() {
        assert_eq!(
            normalize_azure_endpoint("https://myresource.openai.azure.com"),
            "https://myresource.openai.azure.com"
        );
    }

    #[test]
    fn test_azure_url_normalization_with_http() {
        assert_eq!(
            normalize_azure_endpoint("http://myresource.openai.azure.com"),
            "http://myresource.openai.azure.com"
        );
    }

    #[test]
    fn test_azure_url_normalization_trailing_slash() {
        assert_eq!(
            normalize_azure_endpoint("myresource.openai.azure.com/"),
            "https://myresource.openai.azure.com"
        );
    }

    #[test]
    fn test_azure_url_normalization_multiple_trailing_slashes() {
        assert_eq!(
            normalize_azure_endpoint("https://myresource.openai.azure.com///"),
            "https://myresource.openai.azure.com"
        );
    }

    #[test]
    fn test_azure_url_normalization_with_openai_path() {
        assert_eq!(
            normalize_azure_endpoint("https://my.openai.azure.com/openai/deployments/gpt4"),
            "https://my.openai.azure.com"
        );
    }

    #[test]
    fn test_azure_url_normalization_with_openai_deployments_path() {
        assert_eq!(
            normalize_azure_endpoint("https://test.openai.azure.com/openai/deployments/"),
            "https://test.openai.azure.com"
        );
    }

    #[test]
    fn test_azure_url_normalization_openai_in_hostname() {
        assert_eq!(
            normalize_azure_endpoint("https://myresource.openai.azure.com"),
            "https://myresource.openai.azure.com"
        );
    }

    #[test]
    fn test_azure_url_normalization_openai_in_subdomain() {
        assert_eq!(
            normalize_azure_endpoint("https://openai.example.com"),
            "https://openai.example.com"
        );
    }

    #[test]
    fn test_azure_url_normalization_complex_path() {
        assert_eq!(
            normalize_azure_endpoint(
                "myresource.openai.azure.com/openai/deployments/model/chat/completions"
            ),
            "https://myresource.openai.azure.com"
        );
    }

    #[test]
    fn test_azure_url_normalization_path_without_openai() {
        assert_eq!(
            normalize_azure_endpoint("https://myresource.azure.com/api/v1"),
            "https://myresource.azure.com/api/v1"
        );
    }

    #[test]
    fn test_azure_url_normalization_custom_port() {
        assert_eq!(
            normalize_azure_endpoint("https://localhost:8080/openai/deployments"),
            "https://localhost:8080"
        );
    }

    #[test]
    fn test_azure_url_normalization_no_scheme_with_path() {
        assert_eq!(
            normalize_azure_endpoint("myresource.openai.azure.com/openai"),
            "https://myresource.openai.azure.com"
        );
    }

    #[test]
    fn test_is_reasoning_model() {
        assert!(is_reasoning_model("o1-mini"));
        assert!(is_reasoning_model("o3-mini"));
        assert!(is_reasoning_model("gpt-5"));
        assert!(is_reasoning_model("gpt-5-turbo"));
        assert!(!is_reasoning_model("gpt-4o"));
        assert!(!is_reasoning_model("claude-3"));
        assert!(!is_reasoning_model("ollama"));
    }
}
