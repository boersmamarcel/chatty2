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

    /// How this provider reports cached prompt tokens relative to its input
    /// count — see [`crate::services::llm_service::UsageSemantics`] doc for
    /// the two conventions. The match is exhaustive (no `_` arm) so adding a
    /// provider forces an explicit decision here rather than silently
    /// inheriting a default.
    pub fn usage_semantics(&self) -> crate::services::llm_service::UsageSemantics {
        use crate::services::llm_service::UsageSemantics;
        match self {
            // OpenRouter's completion responses are OpenAI-compatible.
            ProviderType::OpenRouter => UsageSemantics::InputIncludesCache,
            // Ollama's /api/chat usage is OpenAI-compatible.
            ProviderType::Ollama => UsageSemantics::InputIncludesCache,
            // Azure OpenAI speaks the OpenAI Chat Completions wire format.
            ProviderType::AzureOpenAI => UsageSemantics::InputIncludesCache,
        }
    }
}

/// The Ollama endpoint used when a provider names none — the same default
/// `provider_builder` falls back to when it builds the client.
pub const DEFAULT_OLLAMA_URL: &str = "http://localhost:11434";

/// OpenRouter's endpoint, which is not configurable per provider today but
/// still needs a stable key to meter (ADR-0011 C6).
pub const DEFAULT_OPENROUTER_URL: &str = "https://openrouter.ai/api/v1";

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

    /// A stable key for the model server this provider talks to.
    ///
    /// The broker budgets concurrency per endpoint rather than per provider
    /// or per model (ADR-0011 C6): two providers pointed at one Ollama
    /// instance are one queue, because they are one process with one set of
    /// loaded weights.
    pub fn endpoint_key(&self) -> String {
        match self
            .base_url
            .as_deref()
            .map(str::trim)
            .filter(|url| !url.is_empty())
        {
            Some(url) => url.trim_end_matches('/').to_string(),
            // Azure has no default endpoint — the deployment URL *is* the
            // configuration — so an unconfigured one is keyed by name rather
            // than pooled with every other unconfigured Azure provider.
            None => match self.provider_type {
                ProviderType::Ollama => DEFAULT_OLLAMA_URL.to_string(),
                ProviderType::OpenRouter => DEFAULT_OPENROUTER_URL.to_string(),
                ProviderType::AzureOpenAI => format!("azure-openai:{}", self.name),
            },
        }
    }

    /// How many requests this endpoint serves in parallel, when it says so.
    ///
    /// `num_parallel` in `extra_config` is the answer whoever configured the
    /// provider gave. Failing that, a local Ollama's `OLLAMA_NUM_PARALLEL`
    /// is read from this process's environment — the desktop and its workers
    /// inherit the same environment as the server when it was started from a
    /// user session, so it is right often enough to be worth reading and
    /// never fabricates a number when it is absent.
    ///
    /// `None` means "nothing known", which the caller turns into its
    /// configured default rather than a guess.
    pub fn parallel_requests(&self) -> Option<usize> {
        self.extra_config
            .get("num_parallel")
            .and_then(|value| value.trim().parse::<usize>().ok())
            .filter(|n| *n > 0)
            .or_else(|| {
                self.ollama_env_parallel(std::env::var("OLLAMA_NUM_PARALLEL").ok().as_deref())
            })
    }

    /// The `OLLAMA_NUM_PARALLEL` half of [`parallel_requests`](Self::parallel_requests),
    /// split out so it can be tested without touching the process
    /// environment.
    fn ollama_env_parallel(&self, raw: Option<&str>) -> Option<usize> {
        if self.provider_type != ProviderType::Ollama || !self.is_loopback_endpoint() {
            return None;
        }
        raw.and_then(|value| value.trim().parse::<usize>().ok())
            .filter(|n| *n > 0)
    }

    /// Whether this provider's endpoint is served by this machine.
    ///
    /// Only then does this process's environment say anything about the
    /// server's settings; a remote Ollama's parallelism is its own business.
    fn is_loopback_endpoint(&self) -> bool {
        let Some(url) = self
            .base_url
            .as_deref()
            .map(str::trim)
            .filter(|url| !url.is_empty())
        else {
            // No URL means the loopback default.
            return true;
        };
        let host = url
            .split_once("://")
            .map_or(url, |(_, rest)| rest)
            .split('/')
            .next()
            .unwrap_or("");
        // An IPv6 host is bracketed, so the port is whatever follows the
        // closing bracket; anything else splits on the last colon.
        let host = match host.find(']') {
            Some(end) => &host[..=end],
            None => host.rsplit_once(':').map_or(host, |(h, _)| h),
        };
        matches!(host, "localhost" | "127.0.0.1" | "0.0.0.0" | "[::1]")
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
    use crate::services::llm_service::UsageSemantics;

    fn ollama(base_url: Option<&str>) -> ProviderConfig {
        let config = ProviderConfig::new("Ollama".to_string(), ProviderType::Ollama);
        match base_url {
            Some(url) => config.with_base_url(url.to_string()),
            None => config,
        }
    }

    #[test]
    fn an_endpoint_key_is_the_base_url_without_its_trailing_slash() {
        assert_eq!(
            ollama(Some("http://box:11434/")).endpoint_key(),
            "http://box:11434"
        );
        assert_eq!(
            ollama(Some("http://box:11434")).endpoint_key(),
            "http://box:11434",
            "two spellings of one server are one queue"
        );
    }

    #[test]
    fn an_unconfigured_endpoint_keys_on_the_default_the_client_would_use() {
        assert_eq!(ollama(None).endpoint_key(), DEFAULT_OLLAMA_URL);
        assert_eq!(
            ProviderConfig::new("OpenRouter".into(), ProviderType::OpenRouter).endpoint_key(),
            DEFAULT_OPENROUTER_URL
        );
        assert_eq!(
            ProviderConfig::new("Prod".into(), ProviderType::AzureOpenAI).endpoint_key(),
            "azure-openai:Prod",
            "Azure has no default endpoint, so its providers are not pooled"
        );
    }

    #[test]
    fn a_configured_parallel_count_is_what_the_provider_reports() {
        let mut config = ollama(Some("http://box:11434"));
        config
            .extra_config
            .insert("num_parallel".to_string(), " 4 ".to_string());
        assert_eq!(config.parallel_requests(), Some(4));

        config
            .extra_config
            .insert("num_parallel".to_string(), "not a number".to_string());
        assert_eq!(
            config.parallel_requests(),
            None,
            "a value that is not a count says nothing, and nothing is not a guess"
        );

        config
            .extra_config
            .insert("num_parallel".to_string(), "0".to_string());
        assert_eq!(config.parallel_requests(), None, "zero would be a deadlock");
    }

    #[test]
    fn ollama_num_parallel_is_read_only_for_a_local_ollama() {
        assert_eq!(ollama(None).ollama_env_parallel(Some("4")), Some(4));
        assert_eq!(
            ollama(Some("http://localhost:11434")).ollama_env_parallel(Some("4")),
            Some(4)
        );
        assert_eq!(
            ollama(Some("http://[::1]:11434")).ollama_env_parallel(Some("4")),
            Some(4)
        );
        assert_eq!(
            ollama(Some("http://box.local:11434")).ollama_env_parallel(Some("4")),
            None,
            "this process's environment says nothing about someone else's server"
        );
        assert_eq!(
            ProviderConfig::new("OpenRouter".into(), ProviderType::OpenRouter)
                .ollama_env_parallel(Some("4")),
            None,
            "OLLAMA_NUM_PARALLEL is Ollama's setting"
        );
        assert_eq!(ollama(None).ollama_env_parallel(None), None);
    }

    #[test]
    fn usage_semantics_is_openai_compatible_for_every_provider() {
        for provider in [
            ProviderType::OpenRouter,
            ProviderType::Ollama,
            ProviderType::AzureOpenAI,
        ] {
            assert_eq!(
                provider.usage_semantics(),
                UsageSemantics::InputIncludesCache
            );
        }
    }

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
