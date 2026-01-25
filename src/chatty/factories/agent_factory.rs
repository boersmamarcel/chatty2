use anyhow::{Result, anyhow};
use rig::agent::Agent;
use rig::client::CompletionClient;

use crate::settings::models::models_store::ModelConfig;
use crate::settings::models::providers_store::{ProviderConfig, ProviderType};

/// Enum-based agent wrapper for multi-provider support
#[derive(Clone)]
pub enum AgentClient {
    Anthropic(Agent<rig::providers::anthropic::completion::CompletionModel>),
    OpenAI(Agent<rig::providers::openai::responses_api::ResponsesCompletionModel>),
    Gemini(Agent<rig::providers::gemini::completion::CompletionModel>),
    Cohere(Agent<rig::providers::cohere::completion::CompletionModel>),
    Ollama(Agent<rig::providers::ollama::CompletionModel>),
}

impl AgentClient {
    /// Create AgentClient from ModelConfig and ProviderConfig
    pub async fn from_model_config(
        model_config: &ModelConfig,
        provider_config: &ProviderConfig,
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

                Ok(AgentClient::Anthropic(builder.build()))
            }
            ProviderType::OpenAI => {
                let key =
                    api_key.ok_or_else(|| anyhow!("API key not configured for OpenAI provider"))?;

                let client = rig::providers::openai::Client::new(&key)?;
                let builder = client
                    .agent(&model_config.model_identifier)
                    .preamble(&model_config.preamble)
                    .temperature(model_config.temperature as f64);

                Ok(AgentClient::OpenAI(builder.build()))
            }
            ProviderType::Gemini => {
                let key =
                    api_key.ok_or_else(|| anyhow!("API key not configured for Gemini provider"))?;

                let client = rig::providers::gemini::Client::new(&key)?;
                let builder = client
                    .agent(&model_config.model_identifier)
                    .preamble(&model_config.preamble)
                    .temperature(model_config.temperature as f64);

                Ok(AgentClient::Gemini(builder.build()))
            }
            ProviderType::Cohere => {
                let key =
                    api_key.ok_or_else(|| anyhow!("API key not configured for Cohere provider"))?;

                let client = rig::providers::cohere::Client::new(&key)?;
                let mut builder = client
                    .agent(&model_config.model_identifier)
                    .preamble(&model_config.preamble)
                    .temperature(model_config.temperature as f64);

                if let Some(max_tokens) = model_config.max_tokens {
                    builder = builder.max_tokens(max_tokens as u64);
                }

                Ok(AgentClient::Cohere(builder.build()))
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

                Ok(AgentClient::Ollama(builder.build()))
            }
            _ => Err(anyhow!(
                "Unsupported provider type: {:?}",
                provider_config.provider_type
            )),
        }
    }
}
