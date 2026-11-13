use anyhow::{Result, anyhow};
use rig_core::DynModel;
use rig_core::operation::Embedding;
use rig_core::providers::ollama::OllamaConfig;
use rig_core::providers::openai::OpenAIConfig;
use rig_core::providers::openai::wire::{self as openai, Auth};
use rig_reqwest::ReqwestClient;
use std::sync::Arc;
use tracing::{info, warn};

use crate::factories::agent_factory::azure_auth_http::{AzureAuthHttpClient, BearerTokenSource};
use crate::factories::agent_factory::{AZURE_ENTRA_PLACEHOLDER_TOKEN, shared_azure_token_cache};

use crate::settings::models::providers_store::ProviderType;

/// Service for computing text embeddings using rig-core providers.
///
/// Wraps a rig-core embedding model and provides a simple `embed()` API
/// that returns `Vec<f32>` for direct use with memvid-core's vector index.
///
/// The embedding provider is independent of the chat model provider —
/// e.g. an Anthropic user can use OpenAI for embeddings.
#[derive(Clone)]
pub struct EmbeddingService {
    model: DynModel<Embedding>,
    provider_type: ProviderType,
    model_name: String,
}

impl EmbeddingService {
    /// Create a new EmbeddingService for the given provider and model.
    ///
    /// # Arguments
    /// * `provider_type` — Which provider to use for embeddings
    /// * `model_name` — Embedding model identifier (e.g. "openai/text-embedding-3-small")
    /// * `api_key` — Provider API key (not needed for Ollama or Azure Entra ID)
    /// * `base_url` — Custom endpoint URL (optional; required for Ollama non-default)
    pub fn new(
        provider_type: &ProviderType,
        model_name: &str,
        api_key: Option<&str>,
        base_url: Option<&str>,
    ) -> Result<Self> {
        let http = ReqwestClient::default();
        let model = match provider_type {
            ProviderType::OpenRouter => {
                let key =
                    api_key.ok_or_else(|| anyhow!("API key required for OpenRouter embeddings"))?;
                let mut config = OpenAIConfig::with_key(&openai::OPENROUTER, key.to_string());
                if let Some(url) = base_url {
                    config = config.with_base_url(url);
                }
                config.connect(http).embedding(model_name, None).erase()
            }
            ProviderType::Ollama => {
                let url = base_url.unwrap_or("http://localhost:11434");
                OllamaConfig::new()
                    .with_base_url(url)
                    .connect(http)
                    .embedding(model_name, None)
                    .erase()
            }
            ProviderType::AzureOpenAI => {
                let key = api_key
                    .ok_or_else(|| anyhow!("API key required for Azure OpenAI embeddings"))?;
                let endpoint = base_url
                    .ok_or_else(|| anyhow!("Endpoint URL required for Azure OpenAI embeddings"))?;
                let mut config =
                    OpenAIConfig::with_key(&openai::AZURE, key.to_string()).with_base_url(endpoint);
                config.auth = Auth::ApiKeyHeader;
                config.connect(http).embedding(model_name, None).erase()
            }
        };

        Ok(Self::from_model(model, provider_type, model_name))
    }

    fn from_model(
        model: DynModel<Embedding>,
        provider_type: &ProviderType,
        model_name: &str,
    ) -> Self {
        info!(
            provider = provider_type.display_name(),
            model = model_name,
            "EmbeddingService initialized"
        );

        Self {
            model,
            provider_type: provider_type.clone(),
            model_name: model_name.to_string(),
        }
    }

    /// Create an Azure OpenAI EmbeddingService authenticated with Entra ID.
    ///
    /// The bearer token is asked of `tokens` on every request, so it is
    /// refreshed before it expires instead of being frozen at init (AGE-423).
    pub fn new_azure_entra(
        model_name: &str,
        base_url: &str,
        tokens: Arc<dyn BearerTokenSource>,
    ) -> Result<Self> {
        // rig needs a credential to build the client; the HTTP layer
        // overwrites the header with the current token on every request.
        let mut config =
            OpenAIConfig::with_key(&openai::AZURE, AZURE_ENTRA_PLACEHOLDER_TOKEN.to_string())
                .with_base_url(base_url);
        config.auth = Auth::Bearer;
        let http = AzureAuthHttpClient::new(ReqwestClient::default(), tokens);
        let model = config.connect(http).embedding(model_name, None).erase();
        Ok(Self::from_model(
            model,
            &ProviderType::AzureOpenAI,
            model_name,
        ))
    }

    /// Compute a single text embedding, returning `Vec<f32>` for memvid-core.
    pub async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let embedding = self.model.embed_text(text).await.map_err(|e| {
            anyhow!(
                "{} embedding failed: {e}",
                self.provider_type.display_name()
            )
        })?;

        // Convert Vec<f64> (rig-core) → Vec<f32> (memvid-core)
        Ok(embedding.vec.iter().map(|&v| v as f32).collect())
    }

    /// Returns a stable identifier for memvid-core's `set_vec_model()`.
    ///
    /// Format: "provider:model" (e.g. "OpenRouter:openai/text-embedding-3-small").
    /// If the user changes provider or model, memvid detects the mismatch.
    pub fn model_identifier(&self) -> String {
        format!("{}:{}", self.provider_type.display_name(), self.model_name)
    }

    /// Returns the default embedding model name for a given provider.
    pub fn default_model_for_provider(provider_type: &ProviderType) -> Option<&'static str> {
        match provider_type {
            ProviderType::OpenRouter => Some("openai/text-embedding-3-small"),
            ProviderType::Ollama => Some("nomic-embed-text"),
            ProviderType::AzureOpenAI => Some("text-embedding-3-small"),
        }
    }

    /// Returns whether a provider supports embeddings.
    pub fn provider_supports_embeddings(_provider_type: &ProviderType) -> bool {
        // All three remaining providers support embeddings
        true
    }
}

/// Try to create an EmbeddingService, logging warnings on failure.
/// Returns `None` if the service cannot be initialized.
pub fn try_create_embedding_service(
    provider_type: &ProviderType,
    model_name: &str,
    api_key: Option<&str>,
    base_url: Option<&str>,
) -> Option<EmbeddingService> {
    match EmbeddingService::new(provider_type, model_name, api_key, base_url) {
        Ok(service) => Some(service),
        Err(e) => {
            warn!(
                error = ?e,
                provider = provider_type.display_name(),
                model = model_name,
                "Failed to create EmbeddingService, falling back to BM25-only search"
            );
            None
        }
    }
}

/// Like [`try_create_embedding_service`], for Azure OpenAI with Entra ID auth.
/// Shared by the desktop and terminal frontends. Tokens come from the
/// process-wide [`AzureTokenCache`](crate::auth::AzureTokenCache), which
/// refreshes them ahead of expiry; the first token is fetched here so a bad
/// credential disables semantic search at startup rather than per call.
pub async fn try_create_azure_entra_embedding_service(
    model_name: &str,
    base_url: Option<&str>,
) -> Option<EmbeddingService> {
    let result = async {
        let base_url =
            base_url.ok_or_else(|| anyhow!("Endpoint URL required for Azure OpenAI embeddings"))?;
        let cache = shared_azure_token_cache()?;
        cache.get_token().await?;
        EmbeddingService::new_azure_entra(model_name, base_url, Arc::new(cache))
    }
    .await;
    match result {
        Ok(service) => Some(service),
        Err(e) => {
            warn!(
                error = ?e,
                model = model_name,
                "Failed to create Entra ID EmbeddingService, falling back to BM25-only search"
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::future::BoxFuture;
    use std::sync::Mutex;
    use wiremock::matchers::{method, path_regex};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Fake token source: a new token each time `expire()` is called.
    struct FakeTokens(Mutex<u32>);

    impl FakeTokens {
        fn expire(&self) {
            *self.0.lock().unwrap() += 1;
        }
    }

    impl BearerTokenSource for FakeTokens {
        fn bearer_token(&self) -> BoxFuture<'_, Result<String>> {
            let n = *self.0.lock().unwrap();
            Box::pin(async move { Ok(format!("token-{n}")) })
        }
    }

    #[tokio::test]
    async fn entra_embeddings_carry_the_token_current_at_each_call() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path_regex("embeddings"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "object": "list",
                "data": [{"object": "embedding", "index": 0, "embedding": [0.1, 0.2]}],
                "model": "m",
                "usage": {"prompt_tokens": 1, "total_tokens": 1}
            })))
            .mount(&server)
            .await;
        let tokens = Arc::new(FakeTokens(Mutex::new(1)));
        let svc = EmbeddingService::new_azure_entra("m", &server.uri(), tokens.clone()).unwrap();

        svc.embed("a").await.unwrap();
        tokens.expire();
        svc.embed("b").await.unwrap();

        let auth: Vec<String> = server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .map(|r| {
                r.headers
                    .get("authorization")
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert_eq!(auth, vec!["Bearer token-1", "Bearer token-2"]);
    }
}
