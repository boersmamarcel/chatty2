use anyhow::{Result, anyhow};
use rig_core::DynModel;
use rig_core::operation::Embedding;
use rig_core::providers::ollama::OllamaConfig;
use rig_core::providers::openai::OpenAIConfig;
use rig_core::providers::openai::wire::{self as openai, Auth};
use rig_reqwest::ReqwestClient;
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
    /// * `azure_token` — Bearer token for Azure Entra ID authentication (Azure only)
    pub fn new(
        provider_type: &ProviderType,
        model_name: &str,
        api_key: Option<&str>,
        base_url: Option<&str>,
        azure_token: Option<String>,
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
                let (credential, auth) = if let Some(token) = azure_token {
                    (token, Auth::Bearer)
                } else if let Some(key) = api_key {
                    (key.to_string(), Auth::ApiKeyHeader)
                } else {
                    return Err(anyhow!(
                        "API key or Entra ID token required for Azure OpenAI embeddings"
                    ));
                };
                let endpoint = base_url
                    .ok_or_else(|| anyhow!("Endpoint URL required for Azure OpenAI embeddings"))?;
                let mut config =
                    OpenAIConfig::with_key(&openai::AZURE, credential).with_base_url(endpoint);
                config.auth = auth;
                config.connect(http).embedding(model_name, None).erase()
            }
        };

        info!(
            provider = provider_type.display_name(),
            model = model_name,
            "EmbeddingService initialized"
        );

        Ok(Self {
            model,
            provider_type: provider_type.clone(),
            model_name: model_name.to_string(),
        })
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
///
/// Pass `azure_token` when using Azure OpenAI with Entra ID authentication.
/// For API key authentication (all other providers and Azure API key), pass `None`.
pub fn try_create_embedding_service(
    provider_type: &ProviderType,
    model_name: &str,
    api_key: Option<&str>,
    base_url: Option<&str>,
    azure_token: Option<String>,
) -> Option<EmbeddingService> {
    match EmbeddingService::new(provider_type, model_name, api_key, base_url, azure_token) {
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
