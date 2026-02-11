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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_azure_auth_method_default() {
        // Provider without auth_method in extra_config should default to ApiKey
        let provider = ProviderConfig::new("test".to_string(), ProviderType::AzureOpenAI);
        assert_eq!(provider.azure_auth_method(), AzureAuthMethod::ApiKey);
    }

    #[test]
    fn test_azure_auth_method_api_key() {
        // Provider with explicit "api_key" value
        let mut provider = ProviderConfig::new("test".to_string(), ProviderType::AzureOpenAI);
        provider
            .extra_config
            .insert("auth_method".to_string(), "api_key".to_string());
        assert_eq!(provider.azure_auth_method(), AzureAuthMethod::ApiKey);
    }

    #[test]
    fn test_azure_auth_method_entra_id() {
        // Provider with explicit "entra_id" value
        let mut provider = ProviderConfig::new("test".to_string(), ProviderType::AzureOpenAI);
        provider
            .extra_config
            .insert("auth_method".to_string(), "entra_id".to_string());
        assert_eq!(provider.azure_auth_method(), AzureAuthMethod::EntraId);
    }

    #[test]
    fn test_azure_auth_method_invalid_value() {
        // Invalid value should default to ApiKey
        let mut provider = ProviderConfig::new("test".to_string(), ProviderType::AzureOpenAI);
        provider
            .extra_config
            .insert("auth_method".to_string(), "invalid".to_string());
        assert_eq!(provider.azure_auth_method(), AzureAuthMethod::ApiKey);
    }

    #[test]
    fn test_set_azure_auth_method_api_key() {
        let mut provider = ProviderConfig::new("test".to_string(), ProviderType::AzureOpenAI);
        provider.set_azure_auth_method(AzureAuthMethod::ApiKey);
        assert_eq!(
            provider.extra_config.get("auth_method"),
            Some(&"api_key".to_string())
        );
        assert_eq!(provider.azure_auth_method(), AzureAuthMethod::ApiKey);
    }

    #[test]
    fn test_set_azure_auth_method_entra_id() {
        let mut provider = ProviderConfig::new("test".to_string(), ProviderType::AzureOpenAI);
        provider.set_azure_auth_method(AzureAuthMethod::EntraId);
        assert_eq!(
            provider.extra_config.get("auth_method"),
            Some(&"entra_id".to_string())
        );
        assert_eq!(provider.azure_auth_method(), AzureAuthMethod::EntraId);
    }

    #[test]
    fn test_configured_providers_azure_with_api_key() {
        let mut model = ProviderModel::new();
        let mut provider = ProviderConfig::new("test".to_string(), ProviderType::AzureOpenAI);
        provider.base_url = Some("https://test.openai.azure.com".to_string());
        provider.api_key = Some("test-key".to_string());
        model.add_provider(provider);

        let configured = model.configured_providers();
        assert_eq!(configured.len(), 1);
        assert_eq!(configured[0].name, "test");
    }

    #[test]
    fn test_configured_providers_azure_with_entra_id() {
        let mut model = ProviderModel::new();
        let mut provider = ProviderConfig::new("test".to_string(), ProviderType::AzureOpenAI);
        provider.base_url = Some("https://test.openai.azure.com".to_string());
        provider.set_azure_auth_method(AzureAuthMethod::EntraId);
        model.add_provider(provider);

        let configured = model.configured_providers();
        assert_eq!(configured.len(), 1);
        assert_eq!(configured[0].name, "test");
    }

    #[test]
    fn test_configured_providers_azure_missing_endpoint() {
        let mut model = ProviderModel::new();
        let mut provider = ProviderConfig::new("test".to_string(), ProviderType::AzureOpenAI);
        // No base_url set
        provider.api_key = Some("test-key".to_string());
        model.add_provider(provider);

        let configured = model.configured_providers();
        assert_eq!(configured.len(), 0); // Should be filtered out
    }

    #[test]
    fn test_configured_providers_azure_missing_credentials() {
        let mut model = ProviderModel::new();
        let mut provider = ProviderConfig::new("test".to_string(), ProviderType::AzureOpenAI);
        provider.base_url = Some("https://test.openai.azure.com".to_string());
        // No API key and no Entra ID
        model.add_provider(provider);

        let configured = model.configured_providers();
        assert_eq!(configured.len(), 0); // Should be filtered out
    }

    #[test]
    fn test_provider_type_default_capabilities() {
        assert_eq!(ProviderType::Anthropic.default_capabilities(), (true, true));
        assert_eq!(ProviderType::Gemini.default_capabilities(), (true, true));
        assert_eq!(ProviderType::OpenAI.default_capabilities(), (true, false));
        assert_eq!(
            ProviderType::AzureOpenAI.default_capabilities(),
            (true, false)
        );
        assert_eq!(ProviderType::Ollama.default_capabilities(), (false, false));
        assert_eq!(ProviderType::Mistral.default_capabilities(), (false, false));
    }

    #[test]
    fn test_provider_type_display_name() {
        assert_eq!(ProviderType::OpenAI.display_name(), "OpenAI");
        assert_eq!(ProviderType::Anthropic.display_name(), "Anthropic");
        assert_eq!(ProviderType::Gemini.display_name(), "Google Gemini");
        assert_eq!(ProviderType::Mistral.display_name(), "Mistral");
        assert_eq!(ProviderType::Ollama.display_name(), "Ollama");
        assert_eq!(ProviderType::AzureOpenAI.display_name(), "Azure OpenAI");
    }
}
