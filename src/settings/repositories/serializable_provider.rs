use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::persistence_error::ProviderPersistenceError;
use crate::settings::models::providers_store::{ProviderConfig, ProviderType};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SerializableProviderType {
    OpenAI,
    Anthropic,
    Gemini,
    Cohere,
    Perplexity,
    #[serde(rename = "xai")]
    XAI,
    AzureOpenAI,
    HuggingFace,
    Ollama,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SerializableProviderConfig {
    pub name: String,
    pub provider_type: SerializableProviderType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub extra_config: HashMap<String, String>,
}

// Conversion implementations
impl From<ProviderType> for SerializableProviderType {
    fn from(pt: ProviderType) -> Self {
        match pt {
            ProviderType::OpenAI => SerializableProviderType::OpenAI,
            ProviderType::Anthropic => SerializableProviderType::Anthropic,
            ProviderType::Gemini => SerializableProviderType::Gemini,
            ProviderType::Cohere => SerializableProviderType::Cohere,
            ProviderType::Perplexity => SerializableProviderType::Perplexity,
            ProviderType::XAI => SerializableProviderType::XAI,
            ProviderType::AzureOpenAI => SerializableProviderType::AzureOpenAI,
            ProviderType::HuggingFace => SerializableProviderType::HuggingFace,
            ProviderType::Ollama => SerializableProviderType::Ollama,
        }
    }
}

impl From<SerializableProviderType> for ProviderType {
    fn from(spt: SerializableProviderType) -> Self {
        match spt {
            SerializableProviderType::OpenAI => ProviderType::OpenAI,
            SerializableProviderType::Anthropic => ProviderType::Anthropic,
            SerializableProviderType::Gemini => ProviderType::Gemini,
            SerializableProviderType::Cohere => ProviderType::Cohere,
            SerializableProviderType::Perplexity => ProviderType::Perplexity,
            SerializableProviderType::XAI => ProviderType::XAI,
            SerializableProviderType::AzureOpenAI => ProviderType::AzureOpenAI,
            SerializableProviderType::HuggingFace => ProviderType::HuggingFace,
            SerializableProviderType::Ollama => ProviderType::Ollama,
        }
    }
}

impl From<ProviderConfig> for SerializableProviderConfig {
    fn from(config: ProviderConfig) -> Self {
        Self {
            name: config.name,
            provider_type: config.provider_type.into(),
            api_key: config.api_key,
            base_url: config.base_url,
            extra_config: config.extra_config,
        }
    }
}

impl TryFrom<SerializableProviderConfig> for ProviderConfig {
    type Error = ProviderPersistenceError;

    fn try_from(config: SerializableProviderConfig) -> Result<Self, Self::Error> {
        Ok(ProviderConfig {
            name: config.name,
            provider_type: config.provider_type.into(),
            api_key: config.api_key,
            base_url: config.base_url,
            extra_config: config.extra_config,
        })
    }
}
