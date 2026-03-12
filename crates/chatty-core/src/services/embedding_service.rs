use anyhow::{Result, anyhow};
use rig::client::EmbeddingsClient;
use rig::embeddings::EmbeddingModel;
use tracing::{info, warn};

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
    inner: EmbeddingServiceInner,
    provider_type: ProviderType,
    model_name: String,
}

/// Concrete embedding model, one variant per supported provider.
#[derive(Clone)]
enum EmbeddingServiceInner {
    OpenAI(rig::providers::openai::embedding::EmbeddingModel),
    Gemini(rig::providers::gemini::embedding::EmbeddingModel),
    Ollama(rig::providers::ollama::EmbeddingModel),
    Mistral(rig::providers::mistral::embedding::EmbeddingModel),
    AzureOpenAI(rig::providers::azure::EmbeddingModel),
}

impl EmbeddingService {
    /// Create a new EmbeddingService for the given provider and model.
    ///
    /// # Arguments
    /// * `provider_type` — Which provider to use for embeddings
    /// * `model_name` — Embedding model identifier (e.g. "text-embedding-3-small")
    /// * `api_key` — Provider API key (not needed for Ollama)
    /// * `base_url` — Custom endpoint URL (optional; required for Ollama non-default)
    pub fn new(
        provider_type: &ProviderType,
        model_name: &str,
        api_key: Option<&str>,
        base_url: Option<&str>,
    ) -> Result<Self> {
        let inner = match provider_type {
            ProviderType::OpenAI => {
                let key =
                    api_key.ok_or_else(|| anyhow!("API key required for OpenAI embeddings"))?;
                let client = rig::providers::openai::Client::new(key)?;
                let model = client.embedding_model(model_name);
                EmbeddingServiceInner::OpenAI(model)
            }
            ProviderType::Gemini => {
                let key =
                    api_key.ok_or_else(|| anyhow!("API key required for Gemini embeddings"))?;
                let client = rig::providers::gemini::Client::new(key)?;
                let model = client.embedding_model(model_name);
                EmbeddingServiceInner::Gemini(model)
            }
            ProviderType::Ollama => {
                let url = base_url.unwrap_or("http://localhost:11434");
                let client = rig::providers::ollama::Client::builder()
                    .api_key(rig::client::Nothing)
                    .base_url(url)
                    .build()?;
                let model = client.embedding_model(model_name);
                EmbeddingServiceInner::Ollama(model)
            }
            ProviderType::Mistral => {
                let key =
                    api_key.ok_or_else(|| anyhow!("API key required for Mistral embeddings"))?;
                let client = rig::providers::mistral::Client::new(key)?;
                let model = client.embedding_model(model_name);
                EmbeddingServiceInner::Mistral(model)
            }
            ProviderType::AzureOpenAI => {
                let key = api_key
                    .ok_or_else(|| anyhow!("API key required for Azure OpenAI embeddings"))?;
                let endpoint = base_url
                    .ok_or_else(|| anyhow!("Endpoint URL required for Azure OpenAI embeddings"))?;
                let client = rig::providers::azure::Client::builder()
                    .api_key(rig::providers::azure::AzureOpenAIAuth::ApiKey(
                        key.to_string(),
                    ))
                    .azure_endpoint(endpoint.to_string())
                    .build()
                    .map_err(|e| anyhow!("Failed to build Azure OpenAI client: {e}"))?;
                let model = client.embedding_model(model_name);
                EmbeddingServiceInner::AzureOpenAI(model)
            }
            ProviderType::Anthropic => {
                return Err(anyhow!(
                    "Anthropic does not offer an embedding API. \
                     Configure a secondary provider (e.g. OpenAI, Ollama) for embeddings."
                ));
            }
        };

        info!(
            provider = provider_type.display_name(),
            model = model_name,
            "EmbeddingService initialized"
        );

        Ok(Self {
            inner,
            provider_type: provider_type.clone(),
            model_name: model_name.to_string(),
        })
    }

    /// Compute a single text embedding, returning `Vec<f32>` for memvid-core.
    pub async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let embedding = match &self.inner {
            EmbeddingServiceInner::OpenAI(m) => m
                .embed_text(text)
                .await
                .map_err(|e| anyhow!("OpenAI embedding failed: {e}"))?,
            EmbeddingServiceInner::Gemini(m) => m
                .embed_text(text)
                .await
                .map_err(|e| anyhow!("Gemini embedding failed: {e}"))?,
            EmbeddingServiceInner::Ollama(m) => m
                .embed_text(text)
                .await
                .map_err(|e| anyhow!("Ollama embedding failed: {e}"))?,
            EmbeddingServiceInner::Mistral(m) => m
                .embed_text(text)
                .await
                .map_err(|e| anyhow!("Mistral embedding failed: {e}"))?,
            EmbeddingServiceInner::AzureOpenAI(m) => m
                .embed_text(text)
                .await
                .map_err(|e| anyhow!("Azure OpenAI embedding failed: {e}"))?,
        };

        // Convert Vec<f64> (rig-core) → Vec<f32> (memvid-core)
        Ok(embedding.vec.iter().map(|&v| v as f32).collect())
    }

    /// Returns a stable identifier for memvid-core's `set_vec_model()`.
    ///
    /// Format: "provider:model" (e.g. "OpenAI:text-embedding-3-small").
    /// If the user changes provider or model, memvid detects the mismatch.
    pub fn model_identifier(&self) -> String {
        format!("{}:{}", self.provider_type.display_name(), self.model_name)
    }

    /// Returns the default embedding model name for a given provider.
    pub fn default_model_for_provider(provider_type: &ProviderType) -> Option<&'static str> {
        match provider_type {
            ProviderType::OpenAI => Some("text-embedding-3-small"),
            ProviderType::Gemini => Some("text-embedding-004"),
            ProviderType::Ollama => Some("nomic-embed-text"),
            ProviderType::Mistral => Some("mistral-embed"),
            ProviderType::AzureOpenAI => Some("text-embedding-3-small"),
            ProviderType::Anthropic => None,
        }
    }

    /// Returns whether a provider supports embeddings.
    pub fn provider_supports_embeddings(provider_type: &ProviderType) -> bool {
        !matches!(provider_type, ProviderType::Anthropic)
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
