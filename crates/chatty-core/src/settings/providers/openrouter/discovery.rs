use anyhow::Result;
use serde::{Deserialize, Serialize};
use tracing::debug;

/// A model returned by the OpenRouter `/api/v1/models` endpoint.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct OpenRouterModel {
    pub id: String,
    pub name: String,
    /// Raw description (optional)
    pub description: Option<String>,
    /// Context length in tokens.
    pub context_length: u64,
    /// Architecture details.
    pub architecture: OpenRouterArchitecture,
    /// Pricing per 1 000 000 tokens.
    pub pricing: OpenRouterPricing,
    /// Top-provider metadata
    pub top_provider: OpenRouterTopProvider,
    /// Parameters this model supports
    pub supported_parameters: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct OpenRouterArchitecture {
    pub modality: String,
    #[serde(default, rename = "input_modalities")]
    pub input_modalities: Vec<String>,
    #[serde(default, rename = "output_modalities")]
    pub output_modalities: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct OpenRouterPricing {
    pub prompt: String,
    pub completion: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct OpenRouterTopProvider {
    #[serde(default)]
    pub context_length: Option<u64>,
    #[serde(default)]
    pub max_completion_tokens: Option<u64>,
}

/// Response envelope from OpenRouter.
#[derive(Debug, Deserialize, Serialize)]
pub struct OpenRouterModelsResponse {
    pub data: Vec<OpenRouterModel>,
}

/// Discover every model listed by OpenRouter.
///
/// This is a single unauthenticated GET to `https://openrouter.ai/api/v1/models`.
/// Returns an error only on network / HTTP / JSON failures.
pub async fn discover_openrouter_models() -> Result<Vec<OpenRouterModel>> {
    debug!("Fetching OpenRouter model catalog …");

    let resp = reqwest::get("https://openrouter.ai/api/v1/models").await?;
    if !resp.status().is_success() {
        return Err(anyhow::anyhow!(
            "OpenRouter returned HTTP {}",
            resp.status()
        ));
    }

    let body: OpenRouterModelsResponse = resp.json().await?;
    debug!(count = body.data.len(), "OpenRouter models fetched");

    Ok(body.data)
}

/// Return `true` if the model supports image input.
///
/// OpenRouter accepts images for **every** model — when a model does not
/// natively support vision OpenRouter transparently parses the image and
/// passes the extracted text to the model.  Therefore we conservatively
/// return `true` for all OpenRouter models rather than relying on the
/// `input_modalities` field which is often incomplete on the gateway side.
pub fn model_supports_images(_model: &OpenRouterModel) -> bool {
    true
}

/// Return `true` if the model supports PDF input.
///
/// OpenRouter accepts PDFs for **every** model.  When a model natively
/// supports file input the PDF is passed directly; otherwise OpenRouter
/// parses the file (e.g. with Cloudflare AI or Mistral OCR) and sends
/// the extracted text/markdown to the model.  We therefore always report
/// PDF support unconditionally.
pub fn model_supports_pdf(_model: &OpenRouterModel) -> bool {
    true
}

/// Prompt cost per 1 000 000 tokens (f64).
pub fn model_prompt_cost(model: &OpenRouterModel) -> Option<f64> {
    model.pricing.prompt.parse().ok()
}

/// Completion cost per 1 000 000 tokens (f64).
pub fn model_completion_cost(model: &OpenRouterModel) -> Option<f64> {
    model.pricing.completion.parse().ok()
}
