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
    /// Per-token price of a prompt token served from cache. Absent for
    /// models whose provider does not report cache pricing.
    #[serde(default)]
    pub input_cache_read: Option<String>,
    /// Per-token price of a prompt token written to cache.
    #[serde(default)]
    pub input_cache_write: Option<String>,
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

/// Check that an OpenRouter API key is accepted.
///
/// `/api/v1/models` is public, so a successful fetch says nothing about the
/// key — this asks the authenticated `/api/v1/key` endpoint instead, which is
/// what makes the settings sheet's Test button mean anything.
pub async fn verify_openrouter_key(api_key: &str) -> Result<()> {
    if api_key.trim().is_empty() {
        return Err(anyhow::anyhow!("No API key set"));
    }

    let resp = reqwest::Client::new()
        .get("https://openrouter.ai/api/v1/key")
        .bearer_auth(api_key.trim())
        .send()
        .await?;

    let status = resp.status();
    if status.is_success() {
        Ok(())
    } else if status == reqwest::StatusCode::UNAUTHORIZED
        || status == reqwest::StatusCode::FORBIDDEN
    {
        Err(anyhow::anyhow!("Key rejected"))
    } else {
        Err(anyhow::anyhow!("OpenRouter returned HTTP {status}"))
    }
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

/// Tokens per pricing unit. OpenRouter quotes **per token** (Claude Sonnet 4.6
/// comes back as `"0.000003"`), while every cost field in this app is per
/// million. Forgetting the conversion made every reported cost 1e6 too small.
const TOKENS_PER_MILLION: f64 = 1_000_000.0;

/// Prompt cost per 1 000 000 tokens (f64).
pub fn model_prompt_cost(model: &OpenRouterModel) -> Option<f64> {
    model
        .pricing
        .prompt
        .parse::<f64>()
        .ok()
        .map(|per_token| per_token * TOKENS_PER_MILLION)
}

/// Completion cost per 1 000 000 tokens (f64).
pub fn model_completion_cost(model: &OpenRouterModel) -> Option<f64> {
    model
        .pricing
        .completion
        .parse::<f64>()
        .ok()
        .map(|per_token| per_token * TOKENS_PER_MILLION)
}

/// Cache-read cost per 1 000 000 tokens, when the model reports one.
pub fn model_cache_read_cost(model: &OpenRouterModel) -> Option<f64> {
    model
        .pricing
        .input_cache_read
        .as_deref()
        .and_then(|s| s.parse::<f64>().ok())
        .map(|per_token| per_token * TOKENS_PER_MILLION)
}

/// Cache-write cost per 1 000 000 tokens, when the model reports one.
pub fn model_cache_write_cost(model: &OpenRouterModel) -> Option<f64> {
    model
        .pricing
        .input_cache_write
        .as_deref()
        .and_then(|s| s.parse::<f64>().ok())
        .map(|per_token| per_token * TOKENS_PER_MILLION)
}

#[cfg(test)]
mod pricing_tests {
    use super::*;

    fn model_with_pricing(prompt: &str, completion: &str) -> OpenRouterModel {
        let json = serde_json::json!({
            "id": "test/model",
            "name": "Test",
            "context_length": 1000,
            "architecture": { "modality": "text", "input_modalities": ["text"] },
            "pricing": { "prompt": prompt, "completion": completion },
            "top_provider": {},
            "supported_parameters": [],
        });
        serde_json::from_value(json).expect("fixture parses")
    }

    /// Cache pricing is optional on the wire: a model without it yields `None`
    /// (so the app falls back to the input rate), one with it converts per
    /// token → per million like the other fields.
    #[test]
    fn cache_pricing_is_optional_and_converted_per_million() {
        let without = model_with_pricing("0.000003", "0.000015");
        assert_eq!(model_cache_read_cost(&without), None);
        assert_eq!(model_cache_write_cost(&without), None);

        let json = serde_json::json!({
            "id": "test/model",
            "name": "Test",
            "context_length": 1000,
            "architecture": { "modality": "text", "input_modalities": ["text"] },
            "pricing": {
                "prompt": "0.000003",
                "completion": "0.000015",
                "input_cache_read": "0.0000003",
                "input_cache_write": "0.00000375"
            },
            "top_provider": {},
            "supported_parameters": [],
        });
        let with: OpenRouterModel = serde_json::from_value(json).expect("fixture parses");
        assert!((model_cache_read_cost(&with).unwrap() - 0.3).abs() < 1e-9);
        assert!((model_cache_write_cost(&with).unwrap() - 3.75).abs() < 1e-9);
    }

    /// OpenRouter quotes per token; the app stores per million. Sonnet 4.6 is
    /// $3.00 / $15.00 per Mtok, which arrives as 0.000003 / 0.000015.
    #[test]
    fn converts_per_token_pricing_to_per_million() {
        let model = model_with_pricing("0.000003", "0.000015");
        assert_eq!(model_prompt_cost(&model), Some(3.0));
        assert_eq!(model_completion_cost(&model), Some(15.0));
    }

    #[test]
    fn free_models_stay_free() {
        let model = model_with_pricing("0", "0");
        assert_eq!(model_prompt_cost(&model), Some(0.0));
        assert_eq!(model_completion_cost(&model), Some(0.0));
    }

    #[test]
    fn unparseable_pricing_is_none_not_zero() {
        let model = model_with_pricing("", "n/a");
        assert_eq!(model_prompt_cost(&model), None);
        assert_eq!(model_completion_cost(&model), None);
    }
}
