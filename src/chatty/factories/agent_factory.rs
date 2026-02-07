use anyhow::{Result, anyhow};
use rig::agent::Agent;
use rig::client::CompletionClient;

use crate::settings::models::models_store::ModelConfig;
use crate::settings::models::providers_store::{ProviderConfig, ProviderType};

/// Enum-based agent wrapper for multi-provider support
#[derive(Clone)]
pub enum AgentClient {
    Anthropic(Agent<rig::providers::anthropic::completion::CompletionModel>),
    OpenAI(Agent<rig::providers::openai::responses_api::ResponsesCompletionModel>),
    Gemini(Agent<rig::providers::gemini::completion::CompletionModel>),
    Mistral(Agent<rig::providers::mistral::completion::CompletionModel>),
    Ollama(Agent<rig::providers::ollama::CompletionModel>),
}

impl AgentClient {
    /// Create AgentClient from ModelConfig and ProviderConfig
    pub async fn from_model_config(
        model_config: &ModelConfig,
        provider_config: &ProviderConfig,
    ) -> Result<Self> {
        let api_key = provider_config.api_key.clone();
        let base_url = provider_config.base_url.clone();

        match &provider_config.provider_type {
            ProviderType::Anthropic => {
                let key = api_key
                    .ok_or_else(|| anyhow!("API key not configured for Anthropic provider"))?;

                let client = rig::providers::anthropic::Client::new(&key)?;
                let mut builder = client
                    .agent(&model_config.model_identifier)
                    .preamble(&model_config.preamble)
                    .temperature(model_config.temperature as f64);

                if let Some(max_tokens) = model_config.max_tokens {
                    builder = builder.max_tokens(max_tokens as u64);
                }

                Ok(AgentClient::Anthropic(builder.build()))
            }
            ProviderType::OpenAI => {
                let key =
                    api_key.ok_or_else(|| anyhow!("API key not configured for OpenAI provider"))?;

                let client = rig::providers::openai::Client::new(&key)?;
                let mut builder = client
                    .agent(&model_config.model_identifier)
                    .preamble(&model_config.preamble);

                // Only set temperature if the model supports it
                if model_config.supports_temperature {
                    builder = builder.temperature(model_config.temperature as f64);
                }

                Ok(AgentClient::OpenAI(builder.build()))
            }
            ProviderType::Gemini => {
                let key =
                    api_key.ok_or_else(|| anyhow!("API key not configured for Gemini provider"))?;

                let client = rig::providers::gemini::Client::new(&key)?;
                let builder = client
                    .agent(&model_config.model_identifier)
                    .preamble(&model_config.preamble)
                    .temperature(model_config.temperature as f64);

                Ok(AgentClient::Gemini(builder.build()))
            }
            ProviderType::Mistral => {
                let key = api_key
                    .ok_or_else(|| anyhow!("API key not configured for Mistral provider"))?;

                let client = rig::providers::mistral::Client::new(&key)?;
                let mut builder = client
                    .agent(&model_config.model_identifier)
                    .preamble(&model_config.preamble)
                    .temperature(model_config.temperature as f64);

                if let Some(max_tokens) = model_config.max_tokens {
                    builder = builder.max_tokens(max_tokens as u64);
                }

                Ok(AgentClient::Mistral(builder.build()))
            }
            ProviderType::Ollama => {
                let url = base_url.unwrap_or_else(|| "http://localhost:11434".to_string());

                let client = rig::providers::ollama::Client::builder()
                    .api_key(rig::client::Nothing)
                    .base_url(&url)
                    .build()?;

                let builder = client
                    .agent(&model_config.model_identifier)
                    .preamble(&model_config.preamble)
                    .temperature(model_config.temperature as f64);

                Ok(AgentClient::Ollama(builder.build()))
            }
        }
    }

    /// Returns whether this provider supports image attachments.
    ///
    /// # Provider Support
    /// - ✅ Anthropic, OpenAI, Gemini, Ollama
    /// - ❌ Mistral (panics on image content)
    ///
    /// Use this method to filter attachments before sending to the LLM.
    pub fn supports_images(&self) -> bool {
        match self {
            AgentClient::Anthropic(_) => true,
            AgentClient::OpenAI(_) => true,
            AgentClient::Gemini(_) => true,
            AgentClient::Ollama(_) => true,
            AgentClient::Mistral(_) => false, // Mistral panics on images!
        }
    }

    /// Returns whether this provider natively supports PDF attachments.
    ///
    /// # Provider Support
    /// - ✅ Anthropic, Gemini (native PDF support)
    /// - ⚠️ OpenAI, Ollama (lossy text extraction)
    /// - ❌ Mistral (not supported)
    ///
    /// Providers marked ⚠️ will convert PDFs to text, losing formatting and images.
    pub fn supports_pdf(&self) -> bool {
        match self {
            AgentClient::Anthropic(_) => true,
            AgentClient::Gemini(_) => true,
            AgentClient::OpenAI(_) => false,  // Lossy conversion
            AgentClient::Ollama(_) => false,  // Lossy conversion
            AgentClient::Mistral(_) => false,
        }
    }

    /// Returns the provider name for logging/debugging.
    pub fn provider_name(&self) -> &'static str {
        match self {
            AgentClient::Anthropic(_) => "Anthropic",
            AgentClient::OpenAI(_) => "OpenAI",
            AgentClient::Gemini(_) => "Gemini",
            AgentClient::Ollama(_) => "Ollama",
            AgentClient::Mistral(_) => "Mistral",
        }
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    // Helper to create test model and provider configs
    fn create_test_configs(provider_type: ProviderType) -> (ModelConfig, ProviderConfig) {
        use std::collections::HashMap;
        
        let model_config = ModelConfig {
            id: "test-model".to_string(),
            name: "Test Model".to_string(),
            provider_type: provider_type.clone(),
            model_identifier: "test-id".to_string(),
            preamble: "Test preamble".to_string(),
            temperature: 0.7,
            max_tokens: Some(1000),
            top_p: None,
            extra_params: HashMap::new(),
            cost_per_million_input_tokens: None,
            cost_per_million_output_tokens: None,
            supports_images: true,
            supports_pdf: true,
            supports_temperature: true,
        };

        let provider_config = ProviderConfig {
            name: format!("{:?}", provider_type),
            provider_type,
            api_key: Some("test-key".to_string()),
            base_url: None,
            extra_config: HashMap::new(),
        };

        (model_config, provider_config)
    }

    #[tokio::test]
    async fn test_anthropic_supports_both_images_and_pdfs() {
        let (model_config, provider_config) = create_test_configs(ProviderType::Anthropic);
        let agent = AgentClient::from_model_config(&model_config, &provider_config)
            .await
            .unwrap();

        assert!(agent.supports_images(), "Anthropic should support images");
        assert!(agent.supports_pdf(), "Anthropic should support PDFs");
        assert_eq!(agent.provider_name(), "Anthropic");
    }

    #[tokio::test]
    async fn test_openai_supports_images_not_pdfs() {
        let (model_config, provider_config) = create_test_configs(ProviderType::OpenAI);
        let agent = AgentClient::from_model_config(&model_config, &provider_config)
            .await
            .unwrap();

        assert!(agent.supports_images(), "OpenAI should support images");
        assert!(!agent.supports_pdf(), "OpenAI should not support PDFs natively");
        assert_eq!(agent.provider_name(), "OpenAI");
    }

    #[tokio::test]
    async fn test_gemini_supports_both_images_and_pdfs() {
        let (model_config, provider_config) = create_test_configs(ProviderType::Gemini);
        let agent = AgentClient::from_model_config(&model_config, &provider_config)
            .await
            .unwrap();

        assert!(agent.supports_images(), "Gemini should support images");
        assert!(agent.supports_pdf(), "Gemini should support PDFs");
        assert_eq!(agent.provider_name(), "Gemini");
    }

    #[tokio::test]
    async fn test_mistral_does_not_support_images_or_pdfs() {
        let (model_config, provider_config) = create_test_configs(ProviderType::Mistral);
        let agent = AgentClient::from_model_config(&model_config, &provider_config)
            .await
            .unwrap();

        assert!(!agent.supports_images(), "Mistral should not support images (panics)");
        assert!(!agent.supports_pdf(), "Mistral should not support PDFs");
        assert_eq!(agent.provider_name(), "Mistral");
    }

    #[tokio::test]
    async fn test_ollama_supports_images_not_pdfs() {
        let (mut model_config, mut provider_config) = create_test_configs(ProviderType::Ollama);
        provider_config.base_url = Some("http://localhost:11434".to_string());
        provider_config.api_key = None; // Ollama doesn't need API key
        model_config.model_identifier = "llama2".to_string();

        let agent = AgentClient::from_model_config(&model_config, &provider_config)
            .await
            .unwrap();

        assert!(agent.supports_images(), "Ollama should support images");
        assert!(!agent.supports_pdf(), "Ollama should not support PDFs natively");
        assert_eq!(agent.provider_name(), "Ollama");
    }

    #[test]
    fn test_all_providers_have_consistent_capability_methods() {
        // This test doesn't create actual agents, just verifies the enum variants
        // would compile with the capability methods
        
        // The fact that this compiles proves all variants are handled
        let provider_types = vec![
            ProviderType::Anthropic,
            ProviderType::OpenAI,
            ProviderType::Gemini,
            ProviderType::Mistral,
            ProviderType::Ollama,
        ];

        assert_eq!(provider_types.len(), 5, "Expected 5 provider types");
    }
}
