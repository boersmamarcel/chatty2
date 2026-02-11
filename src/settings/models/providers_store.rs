use gpui::Global;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AzureAuthMethod {
    #[default]
    ApiKey,
    EntraId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[allow(clippy::upper_case_acronyms)]
pub enum ProviderType {
    OpenAI,
    Anthropic,
    Gemini,
    Mistral,
    Ollama,
    #[serde(rename = "azure_openai")]
    AzureOpenAI,
}

impl ProviderType {
    pub fn display_name(&self) -> &str {
        match self {
            ProviderType::OpenAI => "OpenAI",
            ProviderType::Anthropic => "Anthropic",
            ProviderType::Gemini => "Google Gemini",
            ProviderType::Mistral => "Mistral",
            ProviderType::Ollama => "Ollama",
            ProviderType::AzureOpenAI => "Azure OpenAI",
        }
    }

    /// Returns default (supports_images, supports_pdf) based on provider capabilities
    pub fn default_capabilities(&self) -> (bool, bool) {
        match self {
            ProviderType::Anthropic => (true, true),
            ProviderType::Gemini => (true, true),
            ProviderType::OpenAI => (true, false),
            ProviderType::AzureOpenAI => (true, false),
            ProviderType::Ollama => (false, false),
            ProviderType::Mistral => (false, false),
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

    /// Get Azure authentication method from extra_config
    pub fn azure_auth_method(&self) -> AzureAuthMethod {
        self.extra_config
            .get("auth_method")
            .and_then(|v| match v.as_str() {
                "entra_id" => Some(AzureAuthMethod::EntraId),
                "api_key" => Some(AzureAuthMethod::ApiKey),
                _ => None,
            })
            .unwrap_or(AzureAuthMethod::ApiKey) // Default for backward compatibility
    }

    /// Set Azure authentication method
    pub fn set_azure_auth_method(&mut self, method: AzureAuthMethod) {
        let value = match method {
            AzureAuthMethod::ApiKey => "api_key",
            AzureAuthMethod::EntraId => "entra_id",
        };
        self.extra_config
            .insert("auth_method".to_string(), value.to_string());
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
    /// Replace all providers (used when loading from disk)
    pub fn replace_all(&mut self, providers: Vec<ProviderConfig>) {
        self.providers = providers;
    }

    /// Get providers that are configured (have API key or are Ollama)
    pub fn configured_providers(&self) -> Vec<&ProviderConfig> {
        self.providers
            .iter()
            .filter(|p| match p.provider_type {
                // Include Ollama regardless of API key
                ProviderType::Ollama => true,
                // Azure requires endpoint URL AND (API key OR Entra ID)
                ProviderType::AzureOpenAI => {
                    let has_endpoint = p.base_url.as_ref().is_some_and(|u| !u.trim().is_empty());
                    let has_api_key = p.api_key.as_ref().is_some_and(|k| !k.trim().is_empty());
                    let uses_entra_id = p.azure_auth_method() == AzureAuthMethod::EntraId;

                    has_endpoint && (has_api_key || uses_entra_id)
                }
                // Include others only if they have a non-empty API key
                _ => p.api_key.as_ref().is_some_and(|key| !key.trim().is_empty()),
            })
            .collect()
    }
}
