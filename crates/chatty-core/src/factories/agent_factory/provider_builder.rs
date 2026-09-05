//! Provider-specific agent construction.
//!
//! Encapsulates the logic that differs between LLM providers: client creation,
//! builder configuration (temperature, reasoning hints, max tokens), and any
//! provider-specific schema sanitization (e.g. OpenAI `"format"` stripping).

use std::collections::HashSet;
use std::sync::{Arc, OnceLock};

use anyhow::{Context, Result, anyhow};
use rig_agent::agent::AgentBuilder;
use rig_agent::client::AgentClientExt;
use rig_core::client::CompletionClient;
use rig_core::providers::azure::AzureOpenAIAuth;

use crate::auth::AzureTokenCache;
use crate::services::AgentTaskController;
use crate::settings::models::models_store::{AZURE_DEFAULT_API_VERSION, ModelConfig};
use crate::settings::models::providers_store::{AzureAuthMethod, ProviderConfig, ProviderType};

use super::AgentClient;
use super::azure_auth_http::AzureAuthHttpClient;
use super::mcp_helpers::{build_with_mcp_tools, sanitize_mcp_tools_for_openai};
use super::prompt_cache_http::PromptCachingHttpClient;
use super::tool_collector::NativeTools;

static AZURE_TOKEN_CACHE: OnceLock<Option<AzureTokenCache>> = OnceLock::new();

/// What rig is told the Entra credential is. It never reaches Azure:
/// `AzureAuthHttpClient` overwrites the header on every request (AGE-245).
const AZURE_ENTRA_PLACEHOLDER_TOKEN: &str = "entra-token-attached-per-request";

type McpToolSet = Vec<(String, Vec<rmcp::model::Tool>, rmcp::service::ServerSink)>;

/// Build a provider-specific `AgentClient` from pre-collected native tools.
///
/// All tool construction is done before this function — it only handles
/// provider client creation, builder configuration, and MCP attachment.
pub(super) async fn build_provider_agent(
    model_config: &ModelConfig,
    provider_config: &ProviderConfig,
    preamble: &str,
    native_tools: NativeTools,
    mcp_tools: Option<McpToolSet>,
    native_tool_names: &HashSet<String>,
    task_controller: AgentTaskController,
) -> Result<AgentClient> {
    let api_key = provider_config.api_key.clone();
    let base_url = provider_config.base_url.clone();

    match &provider_config.provider_type {
        ProviderType::OpenRouter => {
            let key =
                api_key.ok_or_else(|| anyhow!("API key not configured for OpenRouter provider"))?;

            // Explicit prompt-cache opt-in (AGE-205). Anthropic models behind
            // OpenRouter cache nothing unless the request carries
            // `cache_control` breakpoints; rig's `with_prompt_caching()` puts
            // one on the system message (preamble + tools), and the HTTP
            // layer below adds a moving one on the latest message so the
            // conversation history caches across turns as well. OpenAI-family
            // models ignore the markers and keep caching automatically on the
            // shared prefix.
            let mut builder = rig_core::providers::openrouter::Client::builder()
                .api_key(&key)
                .http_client(PromptCachingHttpClient::new(reqwest::Client::new()));
            if let Some(ref url) = base_url {
                builder = builder.base_url(url);
            }
            let client = builder.build()?;

            let model = client
                .completion_model(&model_config.model_identifier)
                .with_prompt_caching();
            let mut builder = AgentBuilder::new(model).preamble(preamble);

            if model_config.supports_temperature {
                builder = builder.temperature(model_config.temperature as f64);
            }

            if let Some(max_tokens) = model_config.max_tokens {
                builder = builder.max_tokens(max_tokens as u64);
            }

            let mcp_tools = sanitize_mcp_tools_for_openai(mcp_tools);
            let builder = native_tools.apply_to_builder(builder);
            let agent = build_with_mcp_tools!(builder, mcp_tools, native_tool_names);
            Ok(AgentClient {
                agent,
                task_controller,
                provider: ProviderType::OpenRouter,
            })
        }
        ProviderType::Ollama => {
            let url = base_url.unwrap_or_else(|| "http://localhost:11434".to_string());

            let client = rig_core::providers::ollama::Client::builder()
                .api_key(rig_core::client::Nothing)
                .base_url(&url)
                .build()?;

            let builder = client
                .agent(&model_config.model_identifier)
                .preamble(preamble)
                .temperature(model_config.temperature as f64);

            let builder = native_tools.apply_to_builder(builder);
            let agent = build_with_mcp_tools!(builder, mcp_tools, native_tool_names);
            Ok(AgentClient {
                agent,
                task_controller,
                provider: ProviderType::Ollama,
            })
        }
        ProviderType::AzureOpenAI => {
            build_azure_agent(
                model_config,
                provider_config,
                preamble,
                native_tools,
                mcp_tools,
                native_tool_names,
                task_controller,
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
    native_tools: NativeTools,
    mcp_tools: Option<McpToolSet>,
    native_tool_names: &HashSet<String>,
    task_controller: AgentTaskController,
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
            "Invalid Azure endpoint URL (must start with http:// or https://): '{}'",
            endpoint
        ));
    }

    tracing::info!(
        endpoint = %endpoint,
        deployment = %model_config.model_identifier,
        api_version = %api_version,
        auth_method = ?provider_config.azure_auth_method(),
        "Building Azure OpenAI client"
    );

    let client_error = |e: rig_core::http_client::Error| {
        anyhow!(
            "Failed to build Azure client with endpoint '{}': {}",
            endpoint,
            e
        )
    };

    // The two auth methods build differently typed clients; both hand back
    // the same type-erased `AgentBuilder`.
    let builder = match provider_config.azure_auth_method() {
        AzureAuthMethod::EntraId => {
            tracing::info!("Using Entra ID authentication; the token is attached per request");

            let cache = match AZURE_TOKEN_CACHE.get_or_init(|| match AzureTokenCache::new() {
                Ok(cache) => Some(cache),
                Err(e) => {
                    tracing::warn!(error = ?e, "Failed to create the shared Azure token cache");
                    None
                }
            }) {
                Some(cache) => cache.clone(),
                // The shared cache stays `None` for the process once creation
                // failed; a per-agent cache is the same thing without the sharing.
                None => AzureTokenCache::new().context("Failed to create Azure token cache")?,
            };

            // rig writes this placeholder into the bearer header at build time;
            // `AzureAuthHttpClient` replaces it with a current token on every
            // request, so an expired token is refreshed without a rebuild.
            let client = rig_core::providers::azure::Client::builder()
                .api_key(AzureOpenAIAuth::Token(
                    AZURE_ENTRA_PLACEHOLDER_TOKEN.to_string(),
                ))
                .http_client(AzureAuthHttpClient::new(
                    reqwest::Client::new(),
                    Arc::new(cache),
                ))
                .azure_endpoint(endpoint.clone())
                .api_version(api_version)
                .build()
                .map_err(client_error)?;
            client.agent(&model_config.model_identifier)
        }
        AzureAuthMethod::ApiKey => {
            tracing::info!("Using API Key authentication for Azure OpenAI");
            let key = api_key
                .ok_or_else(|| anyhow!("API key not configured for Azure OpenAI provider"))?;

            let client = rig_core::providers::azure::Client::builder()
                .api_key(AzureOpenAIAuth::ApiKey(key))
                .azure_endpoint(endpoint.clone())
                .api_version(api_version)
                .build()
                .map_err(client_error)?;
            client.agent(&model_config.model_identifier)
        }
    };

    let mut builder = builder.preamble(preamble);

    if model_config.supports_temperature {
        builder = builder.temperature(model_config.temperature as f64);
    }

    if let Some(max_tokens) = model_config.max_tokens {
        builder = builder.max_tokens(max_tokens as u64);
    }

    let mcp_tools = sanitize_mcp_tools_for_openai(mcp_tools);
    let builder = native_tools.apply_to_builder(builder);
    let agent = build_with_mcp_tools!(builder, mcp_tools, native_tool_names);
    Ok(AgentClient {
        agent,
        task_controller,
        provider: ProviderType::AzureOpenAI,
    })
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
}
