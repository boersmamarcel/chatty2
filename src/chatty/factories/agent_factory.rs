use anyhow::{Result, anyhow};
use rig::agent::Agent;
use rig::client::CompletionClient;

use crate::settings::models::models_store::{AZURE_DEFAULT_API_VERSION, ModelConfig};
use crate::settings::models::providers_store::{ProviderConfig, ProviderType};

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
    /// Create AgentClient from ModelConfig and ProviderConfig with optional MCP tools
    pub async fn from_model_config_with_tools(
        model_config: &ModelConfig,
        provider_config: &ProviderConfig,
        mcp_tools: Option<Vec<(Vec<rmcp::model::Tool>, rmcp::service::ServerSink)>>,
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

                let agent = build_with_mcp_tools!(builder, mcp_tools);

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

                let agent = build_with_mcp_tools!(builder, mcp_tools);

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

                let agent = build_with_mcp_tools!(builder, mcp_tools);

                Ok(AgentClient::Ollama(agent))
            }
            ProviderType::AzureOpenAI => {
                let key = api_key
                    .ok_or_else(|| anyhow!("API key not configured for Azure OpenAI provider"))?;
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

                // If the endpoint contains /openai/deployments, extract just the base URL
                // Azure endpoint should be just https://myresource.openai.azure.com
                // rig-core appends /openai/deployments/{model}/chat/completions itself
                if let Some(pos) = endpoint.find("/openai/deployments") {
                    endpoint.truncate(pos);
                } else if let Some(pos) = endpoint.find("/openai") {
                    endpoint.truncate(pos);
                }
                let api_version = model_config
                    .extra_params
                    .get("api_version")
                    .map(|s| s.as_str())
                    .unwrap_or(AZURE_DEFAULT_API_VERSION);

                let auth = rig::providers::azure::AzureOpenAIAuth::ApiKey(key);

                // Log the normalized endpoint for debugging
                tracing::info!(
                    endpoint = %endpoint,
                    deployment = %model_config.model_identifier,
                    api_version = %api_version,
                    "Building Azure OpenAI client"
                );

                let client = rig::providers::azure::Client::builder()
                    .api_key(auth)
                    .azure_endpoint(endpoint)
                    .api_version(api_version)
                    .build()?;
                let mut builder = client
                    .agent(&model_config.model_identifier)
                    .preamble(&model_config.preamble);

                if model_config.supports_temperature {
                    builder = builder.temperature(model_config.temperature as f64);
                }

                if let Some(max_tokens) = model_config.max_tokens {
                    builder = builder.max_tokens(max_tokens as u64);
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
