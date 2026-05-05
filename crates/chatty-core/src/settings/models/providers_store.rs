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
    /// OpenRouter — gateway to 200+ models (Anthropic, Google, Mistral, Meta, etc.)
    /// Accepts legacy JSON values from removed provider variants for backward compatibility.
    #[serde(
        alias = "open_ai",
        alias = "open_a_i",
        alias = "anthropic",
        alias = "gemini",
        alias = "mistral"
    )]
    OpenRouter,
    Ollama,
    #[serde(rename = "azure_openai")]
    AzureOpenAI,
}

impl ProviderType {
    pub fn display_name(&self) -> &str {
        match self {
            ProviderType::OpenRouter => "OpenRouter",
            ProviderType::Ollama => "Ollama",
            ProviderType::AzureOpenAI => "Azure OpenAI",
        }
    }

    /// Returns default (supports_images, supports_pdf) based on provider capabilities
    pub fn default_capabilities(&self) -> (bool, bool) {
        match self {
            // OpenRouter is a gateway to multimodal models (Anthropic, Google, etc.)
            ProviderType::OpenRouter => (true, true),
            ProviderType::AzureOpenAI => (true, false),
            ProviderType::Ollama => (false, false),
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

    /// Get providers that are configured (have API key or are Ollama).
    ///
    /// Returns an iterator to avoid allocating a `Vec` on every call.
    /// Callers that need indexed access or multiple passes should `.collect()`.
    pub fn configured_providers(&self) -> impl Iterator<Item = &ProviderConfig> {
        self.providers.iter().filter(|p| match p.provider_type {
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

        let configured: Vec<_> = model.configured_providers().collect();
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

        let configured: Vec<_> = model.configured_providers().collect();
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

        assert_eq!(model.configured_providers().count(), 0); // Should be filtered out
    }

    #[test]
    fn test_configured_providers_azure_missing_credentials() {
        let mut model = ProviderModel::new();
        let mut provider = ProviderConfig::new("test".to_string(), ProviderType::AzureOpenAI);
        provider.base_url = Some("https://test.openai.azure.com".to_string());
        // No API key and no Entra ID
        model.add_provider(provider);

        assert_eq!(model.configured_providers().count(), 0); // Should be filtered out
    }

    #[test]
    fn test_provider_type_default_capabilities() {
        assert_eq!(
            ProviderType::OpenRouter.default_capabilities(),
            (true, true)
        );
        assert_eq!(
            ProviderType::AzureOpenAI.default_capabilities(),
            (true, false)
        );
        assert_eq!(ProviderType::Ollama.default_capabilities(), (false, false));
    }

    #[test]
    fn test_provider_type_display_name() {
        assert_eq!(ProviderType::OpenRouter.display_name(), "OpenRouter");
        assert_eq!(ProviderType::Ollama.display_name(), "Ollama");
        assert_eq!(ProviderType::AzureOpenAI.display_name(), "Azure OpenAI");
    }

    #[test]
    fn test_provider_type_backward_compat_deserialization() {
        // Old JSON values for removed providers should deserialize as OpenRouter
        let openai: ProviderType = serde_json::from_str("\"open_ai\"").unwrap();
        assert_eq!(openai, ProviderType::OpenRouter);

        // Legacy typo variant — was written by earlier app versions
        let legacy_openai: ProviderType = serde_json::from_str("\"open_a_i\"").unwrap();
        assert_eq!(legacy_openai, ProviderType::OpenRouter);

        let anthropic: ProviderType = serde_json::from_str("\"anthropic\"").unwrap();
        assert_eq!(anthropic, ProviderType::OpenRouter);

        let gemini: ProviderType = serde_json::from_str("\"gemini\"").unwrap();
        assert_eq!(gemini, ProviderType::OpenRouter);

        let mistral: ProviderType = serde_json::from_str("\"mistral\"").unwrap();
        assert_eq!(mistral, ProviderType::OpenRouter);
    }
}
