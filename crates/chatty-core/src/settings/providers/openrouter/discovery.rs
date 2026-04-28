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
/// OpenRouter **does not** transparently parse images for non-vision models —
/// sending an `image_url` block to a text-only model results in a 404
/// (`No endpoints found that support image input`).  We therefore rely on the
/// `input_modalities` field from the public `/api/v1/models` endpoint and
/// fall back to well-known multimodal model families when that field is empty
/// or incomplete.
pub fn model_supports_images(model: &OpenRouterModel) -> bool {
    // 1. Explicit modality flag from the API
    if model
        .architecture
        .input_modalities
        .iter()
        .any(|m| m.eq_ignore_ascii_case("image"))
    {
        return true;
    }

    // 2. The `modality` field (e.g. "text+image") is often set even when
    //    `input_modalities` is empty on the gateway side.
    let modality = model.architecture.modality.to_lowercase();
    if modality.contains("image") || modality.contains("vision") {
        return true;
    }

    // 3. Fallback: known multimodal families that OpenRouter hosts.
    //    The gateway metadata is sometimes sparse for models that do
    //    accept vision input natively.
    let id = &model.id.to_lowercase();
    id.starts_with("anthropic/claude-3")
        || id.starts_with("google/gemini")
        || id.starts_with("openai/gpt-4o")
        || id.starts_with("openai/gpt-4.5")
        || id.starts_with("openai/gpt-5")
        || id.starts_with("meta-llama/llama-3.2")
        || id.contains("vision")
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
