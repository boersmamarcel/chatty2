use gpui::Global;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderType {
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

impl ProviderType {
    pub fn display_name(&self) -> &str {
        match self {
            ProviderType::OpenAI => "OpenAI",
            ProviderType::Anthropic => "Anthropic",
            ProviderType::Gemini => "Google Gemini",
            ProviderType::Cohere => "Cohere",
            ProviderType::Perplexity => "Perplexity",
            ProviderType::XAI => "xAI",
            ProviderType::AzureOpenAI => "Azure OpenAI",
            ProviderType::HuggingFace => "HuggingFace",
            ProviderType::Ollama => "Ollama",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub name: String,
    pub provider_type: ProviderType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub extra_config: HashMap<String, String>,
}

impl ProviderConfig {
    pub fn new(name: String, provider_type: ProviderType) -> Self {
        Self {
            name,
            provider_type,
            api_key: None,
            base_url: None,
            extra_config: HashMap::new(),
        }
    }

    pub fn with_api_key(mut self, api_key: String) -> Self {
        self.api_key = Some(api_key);
        self
    }

    pub fn with_base_url(mut self, base_url: String) -> Self {
        self.base_url = Some(base_url);
        self
    }
}

#[derive(Clone)]
pub struct ProviderModel {
    providers: Vec<ProviderConfig>,
}

impl Global for ProviderModel {}

impl ProviderModel {
    pub fn new() -> Self {
        Self {
            providers: Vec::new(),
        }
    }

    pub fn add_provider(&mut self, config: ProviderConfig) {
        self.providers.push(config);
    }

    pub fn providers(&self) -> &[ProviderConfig] {
        &self.providers
    }

    pub fn providers_mut(&mut self) -> &mut Vec<ProviderConfig> {
        &mut self.providers
    }
}

impl Default for ProviderModel {
    fn default() -> Self {
        Self::new()
    }
}

impl ProviderModel {
    /// Take a snapshot of current state (for rollback)
    pub fn snapshot(&self) -> Vec<ProviderConfig> {
        self.providers.clone()
    }

    /// Replace all providers (used when loading from disk)
    pub fn replace_all(&mut self, providers: Vec<ProviderConfig>) {
        self.providers = providers;
    }
}
