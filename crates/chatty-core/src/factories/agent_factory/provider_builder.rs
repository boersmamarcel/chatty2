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
use crate::services::context_shaper::ContextShaper;
use crate::services::http_client::llm_client;
use crate::settings::models::models_store::{AZURE_DEFAULT_API_VERSION, ModelConfig};
use crate::settings::models::providers_store::{AzureAuthMethod, ProviderConfig, ProviderType};

use super::AgentClient;
use super::azure_auth_http::AzureAuthHttpClient;
use super::connect_retry_http::ConnectRetryHttpClient;
use super::empty_turn_retry::EmptyTurnRetry;
use super::mcp_helpers::{build_with_mcp_tools, sanitize_mcp_tools_for_openai};
use super::prompt_cache_http::PromptCachingHttpClient;
use super::request_recorder::RequestRecorder;
use super::tool_collector::NativeTools;
use super::tool_loading::ToolLoader;

static AZURE_TOKEN_CACHE: OnceLock<Option<AzureTokenCache>> = OnceLock::new();

/// What rig is told the Entra credential is. It never reaches Azure:
/// `AzureAuthHttpClient` overwrites the header on every request (AGE-245).
const AZURE_ENTRA_PLACEHOLDER_TOKEN: &str = "entra-token-attached-per-request";

type McpToolSet = Vec<(String, Vec<rmcp::model::Tool>, rmcp::service::ServerSink)>;

/// Preamble for the tool-less utility agent (AGE-227): title generation and
/// summarization need a short, direct reply, never a tool call.
const UTILITY_PREAMBLE: &str = "You are a utility model. Reply only with the requested text.";

/// Build a provider-specific `AgentClient` from pre-collected native tools.
///
/// All tool construction is done before this function — it only handles
/// provider client creation, builder configuration, and MCP attachment.
#[allow(clippy::too_many_arguments)]
pub(super) async fn build_provider_agent(
    model_config: &ModelConfig,
    provider_config: &ProviderConfig,
    preamble: &str,
    native_tools: NativeTools,
    mcp_tools: Option<McpToolSet>,
    native_tool_names: &HashSet<String>,
    task_controller: AgentTaskController,
    tool_loader: Option<ToolLoader>,
) -> Result<AgentClient> {
    let api_key = provider_config.api_key.clone();
    let base_url = provider_config.base_url.clone();
    // The context guard (AGE-504) is a hook on the agent, so it exists before
    // the agent does; the factory calibrates it once the agent is built.
    let context_shaper = ContextShaper::for_model(model_config);
    let request_recorder = RequestRecorder::default();

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
                .http_client(PromptCachingHttpClient::new(llm_client().clone()));
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

            if let Some(think) = openai_compat_think(model_config) {
                builder = builder.additional_params(serde_json::json!({
                    "chat_template_kwargs": { "enable_thinking": think }
                }));
            }

            let mcp_tools = sanitize_mcp_tools_for_openai(mcp_tools);
            let builder = chat_agent_builder(
                native_tools,
                builder,
                context_shaper.clone(),
                request_recorder.clone(),
                tool_loader.clone(),
            );
            let agent = build_with_mcp_tools!(builder, mcp_tools, native_tool_names);

            let utility_model = client
                .completion_model(&model_config.model_identifier)
                .with_prompt_caching();
            let mut utility = AgentBuilder::new(utility_model).preamble(UTILITY_PREAMBLE);
            if let Some(params) = openai_compat_utility_params(model_config) {
                utility = utility.additional_params(params);
            }
            let utility = utility.build();

            Ok(AgentClient {
                agent,
                task_controller,
                provider: ProviderType::OpenRouter,
                utility,
                context_shaper,
                request_recorder,
                tool_loader,
                supports_images: model_config.supports_images,
            })
        }
        ProviderType::Ollama => {
            let url = base_url.unwrap_or_else(|| "http://localhost:11434".to_string());

            let client = rig_core::providers::ollama::Client::builder()
                .api_key(rig_core::client::Nothing)
                .base_url(&url)
                .http_client(ConnectRetryHttpClient::new(llm_client().clone()))
                .build()?;

            let mut builder = client
                .agent(&model_config.model_identifier)
                .preamble(preamble);

            if model_config.supports_temperature {
                builder = builder.temperature(model_config.temperature as f64);
            }

            if let Some(max_tokens) = model_config.max_tokens {
                builder = builder.max_tokens(max_tokens as u64);
            }

            if let Some(think) = ollama_think(model_config) {
                builder = builder.additional_params(serde_json::json!({ "think": think }));
            }

            let builder = chat_agent_builder(
                native_tools,
                builder,
                context_shaper.clone(),
                request_recorder.clone(),
                tool_loader.clone(),
            );
            let agent = build_with_mcp_tools!(builder, mcp_tools, native_tool_names);

            let mut utility = client
                .agent(&model_config.model_identifier)
                .preamble(UTILITY_PREAMBLE);
            if let Some(params) = ollama_utility_params(model_config) {
                utility = utility.additional_params(params);
            }
            let utility = utility.build();

            Ok(AgentClient {
                agent,
                task_controller,
                provider: ProviderType::Ollama,
                utility,
                context_shaper,
                request_recorder,
                tool_loader,
                supports_images: model_config.supports_images,
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
                context_shaper,
                request_recorder,
                tool_loader,
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
    context_shaper: ContextShaper,
    request_recorder: RequestRecorder,
    tool_loader: Option<ToolLoader>,
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

    // The two auth methods build differently typed clients; both hand back the
    // same type-erased `AgentBuilder`. Each also builds its own tool-less
    // utility agent (AGE-227) from the client it owns: `client` cannot outlive
    // the arm now that the two arms' client types differ.
    let (builder, utility) = match provider_config.azure_auth_method() {
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
                    ConnectRetryHttpClient::new(llm_client().clone()),
                    Arc::new(cache),
                ))
                .azure_endpoint(endpoint.clone())
                .api_version(api_version)
                .build()
                .map_err(client_error)?;
            let utility = client
                .agent(&model_config.model_identifier)
                .preamble(UTILITY_PREAMBLE)
                .build();
            (client.agent(&model_config.model_identifier), utility)
        }
        AzureAuthMethod::ApiKey => {
            tracing::info!("Using API Key authentication for Azure OpenAI");
            let key = api_key
                .ok_or_else(|| anyhow!("API key not configured for Azure OpenAI provider"))?;

            let client = rig_core::providers::azure::Client::builder()
                .api_key(AzureOpenAIAuth::ApiKey(key))
                .azure_endpoint(endpoint.clone())
                .api_version(api_version)
                .http_client(ConnectRetryHttpClient::new(llm_client().clone()))
                .build()
                .map_err(client_error)?;
            let utility = client
                .agent(&model_config.model_identifier)
                .preamble(UTILITY_PREAMBLE)
                .build();
            (client.agent(&model_config.model_identifier), utility)
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
    let builder = chat_agent_builder(
        native_tools,
        builder,
        context_shaper.clone(),
        request_recorder.clone(),
        tool_loader.clone(),
    );
    let agent = build_with_mcp_tools!(builder, mcp_tools, native_tool_names);

    Ok(AgentClient {
        agent,
        task_controller,
        provider: ProviderType::AzureOpenAI,
        utility,
        context_shaper,
        request_recorder,
        tool_loader,
        supports_images: model_config.supports_images,
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

/// Every chat agent's builder: the native tools, plus the in-turn hooks —
/// the one that retries an empty completion once inside the turn (AGE-401),
/// the context guard that keeps every model call of the run inside the
/// model's window (AGE-504), and the recorder that keeps the last request
/// for a run that fails. Under dynamic tool loading the loader goes ahead of
/// the context guard, so a group it loads is counted in the same call.
/// Utility agents (titles, summaries) carry none of them.
fn chat_agent_builder(
    native_tools: NativeTools,
    builder: AgentBuilder,
    context_shaper: ContextShaper,
    request_recorder: RequestRecorder,
    tool_loader: Option<ToolLoader>,
) -> AgentBuilder<rig_agent::agent::WithBuilderTools> {
    let builder = native_tools
        .apply_to_builder(builder)
        .add_hook(EmptyTurnRetry);
    let builder = match tool_loader {
        Some(loader) => builder.add_hook(loader),
        None => builder,
    };
    builder.add_hook(context_shaper).add_hook(request_recorder)
}

/// Ollama's per-request `think` switch, from the model's `extra_params.think`
/// (`"true"` / `"false"`). rig forwards it as the request's top-level `think`
/// field. A thinking model such as qwen3 sometimes writes its tool call inside
/// the thinking channel, which Ollama never surfaces as a tool call, so the
/// turn ends with empty content; `think=false` on a leader or reviewer roster
/// entry avoids that. Absent or unparsable: the request carries no `think`
/// key and Ollama's model default applies.
pub(crate) fn ollama_think(model_config: &ModelConfig) -> Option<bool> {
    model_config
        .extra_params
        .get("think")
        .and_then(|v| v.trim().parse::<bool>().ok())
}

/// The OpenAI-compat request's reasoning-mode switch, from the same
/// `extra_params.think` field `ollama_think` reads (AGE-455). vLLM and other
/// `--reasoning-parser` OpenAI-compat servers toggle extended thinking via
/// `chat_template_kwargs.enable_thinking` in the request body rather than a
/// top-level field; rig's OpenAI-compat request flattens `additional_params`
/// into the body, so the caller nests it there. Absent or unparsable: no
/// override, so a server without a reasoning parser gets no extra field.
pub(crate) fn openai_compat_think(model_config: &ModelConfig) -> Option<bool> {
    model_config
        .extra_params
        .get("think")
        .and_then(|v| v.trim().parse::<bool>().ok())
}

/// The utility agent's request parameters on the OpenAI-compat arm: thinking
/// off whenever the model's `think` switch is set at all (the server then
/// understands it). Titles and compaction summaries need a short reply, and
/// a summary that thinks first can run into its 90 s timeout on a local
/// model; without a switch nothing is sent, as for the main agent.
pub(crate) fn openai_compat_utility_params(
    model_config: &ModelConfig,
) -> Option<serde_json::Value> {
    openai_compat_think(model_config)
        .map(|_| serde_json::json!({ "chat_template_kwargs": { "enable_thinking": false } }))
}

/// [`openai_compat_utility_params`] for Ollama's `think` field.
pub(crate) fn ollama_utility_params(model_config: &ModelConfig) -> Option<serde_json::Value> {
    ollama_think(model_config).map(|_| serde_json::json!({ "think": false }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_utility_agent_never_thinks_when_the_switch_is_known() {
        let mut model = ModelConfig::new(
            "qwen".into(),
            "qwen".into(),
            ProviderType::OpenRouter,
            "qwen".into(),
        );
        assert_eq!(openai_compat_utility_params(&model), None);
        assert_eq!(ollama_utility_params(&model), None);
        for think in ["true", "false"] {
            model
                .extra_params
                .insert("think".to_string(), think.to_string());
            assert_eq!(
                openai_compat_utility_params(&model),
                Some(serde_json::json!({ "chat_template_kwargs": { "enable_thinking": false } }))
            );
            assert_eq!(
                ollama_utility_params(&model),
                Some(serde_json::json!({ "think": false }))
            );
        }
    }

    /// AGE-227: the utility agent must carry no tools. This mirrors the exact
    /// builder path used for the Ollama arm's `utility` field — no
    /// `.tool()`/`.rmcp_tools()` calls before `.build()` — without needing the
    /// full `NativeTools` tool-collection setup that `build_provider_agent`
    /// requires.
    #[tokio::test]
    async fn utility_agent_builder_path_has_no_tools() {
        let client = rig_core::providers::ollama::Client::builder()
            .api_key(rig_core::client::Nothing)
            .base_url("http://localhost:11434")
            .build()
            .expect("client construction does not make a network call");

        let utility = client
            .agent("test-model")
            .preamble(UTILITY_PREAMBLE)
            .build();

        let defs = utility
            .tool_definitions(None)
            .await
            .expect("tool_definitions with no prompt does no I/O");
        assert!(defs.is_empty(), "utility agent must be built with no tools");
    }

    /// AGE-400: `extra_params.think` becomes Ollama's request-level `think`
    /// switch; anything else leaves the request without one.
    #[test]
    fn ollama_think_reads_extra_params() {
        let mut model = ModelConfig::new(
            "qwen3".into(),
            "qwen3:14b".into(),
            ProviderType::Ollama,
            "qwen3:14b".into(),
        );
        assert_eq!(ollama_think(&model), None);

        model
            .extra_params
            .insert("think".to_string(), "false".to_string());
        assert_eq!(ollama_think(&model), Some(false));

        model
            .extra_params
            .insert("think".to_string(), " true ".to_string());
        assert_eq!(ollama_think(&model), Some(true));

        model
            .extra_params
            .insert("think".to_string(), "maybe".to_string());
        assert_eq!(ollama_think(&model), None);
    }

    /// AGE-455: same field, same parsing, for the OpenAI-compat arm.
    #[test]
    fn openai_compat_think_reads_extra_params() {
        let mut model = ModelConfig::new(
            "compat".into(),
            "some-model".into(),
            ProviderType::OpenRouter,
            "some-model".into(),
        );
        assert_eq!(openai_compat_think(&model), None);

        model
            .extra_params
            .insert("think".to_string(), "false".to_string());
        assert_eq!(openai_compat_think(&model), Some(false));

        model
            .extra_params
            .insert("think".to_string(), "true".to_string());
        assert_eq!(openai_compat_think(&model), Some(true));
    }

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
