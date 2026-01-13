use gpui::Global;
use rig::client::{Nothing, ProviderClient};
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProviderType {
    OpenAI,
    Anthropic,
    Gemini,
    Cohere,
    Perplexity,
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

    pub fn requires_api_key(&self) -> bool {
        !matches!(self, ProviderType::Ollama)
    }
}

#[derive(Clone, Debug)]
pub struct ProviderConfig {
    pub name: String,
    pub provider_type: ProviderType,
    pub api_key: Option<String>,
    pub base_url: Option<String>,
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

    pub fn with_extra(mut self, key: String, value: String) -> Self {
        self.extra_config.insert(key, value);
        self
    }

    /// Create a client dynamically based on the provider type
    pub fn create_client(&self) -> Result<Box<dyn std::any::Any + Send + Sync>, String> {
        // Validate API key requirement
        if self.provider_type.requires_api_key() && self.api_key.is_none() {
            return Err(format!(
                "{} requires an API key",
                self.provider_type.display_name()
            ));
        }

        let api_key = self.api_key.as_deref().unwrap_or("");

        match self.provider_type {
            ProviderType::OpenAI => {
                // Note: This uses environment variable, custom API keys not yet supported
                let client = rig::providers::openai::Client::from_env();
                Ok(Box::new(client))
            }
            ProviderType::Gemini => {
                // Note: This uses environment variable, custom API keys not yet supported
                let client = rig::providers::gemini::Client::from_env();
                Ok(Box::new(client))
            }
            ProviderType::Cohere => {
                // Note: This uses environment variable, custom API keys not yet supported
                let client = rig::providers::cohere::Client::from_env();
                Ok(Box::new(client))
            }
            ProviderType::Perplexity => {
                // Note: This uses environment variable, custom API keys not yet supported
                let client = rig::providers::perplexity::Client::from_env();
                Ok(Box::new(client))
            }
            ProviderType::XAI => {
                // Note: This uses environment variable, custom API keys not yet supported
                let client = rig::providers::xai::Client::from_env();
                Ok(Box::new(client))
            }

            ProviderType::Anthropic => {
                // Note: This uses environment variable, custom API keys not yet supported
                let client = rig::providers::anthropic::Client::from_env();
                Ok(Box::new(client))
            }

            ProviderType::AzureOpenAI => {
                // Note: This uses environment variable, custom API keys not yet supported
                let client = rig::providers::azure::Client::from_env();
                Ok(Box::new(client))
            }

            ProviderType::HuggingFace => {
                // Note: This uses environment variable, custom API keys not yet supported
                let client = rig::providers::huggingface::Client::from_env();
                Ok(Box::new(client))
            }

            ProviderType::Ollama => {
                // TODO: Ollama client creation not yet fully implemented
                Err("Ollama provider not yet supported".to_string())
            }
        }
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

    pub fn get_provider(&self, index: usize) -> Option<&ProviderConfig> {
        self.providers.get(index)
    }

    pub fn remove_provider(&mut self, index: usize) -> Option<ProviderConfig> {
        if index < self.providers.len() {
            Some(self.providers.remove(index))
        } else {
            None
        }
    }

    /// Create a client from a stored provider configuration
    pub fn create_client(
        &self,
        index: usize,
    ) -> Result<Box<dyn std::any::Any + Send + Sync>, String> {
        self.get_provider(index)
            .ok_or_else(|| "Provider not found".to_string())?
            .create_client()
    }
}

impl Default for ProviderModel {
    fn default() -> Self {
        Self::new()
    }
}
