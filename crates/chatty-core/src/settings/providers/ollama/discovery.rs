use anyhow::Result;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

#[derive(Debug, Deserialize, Serialize)]
struct OllamaModel {
    name: String,
    #[serde(default)]
    model: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct OllamaTagsResponse {
    models: Vec<OllamaModel>,
}

#[derive(Debug, Deserialize)]
struct OllamaShowResponse {
    #[serde(default)]
    capabilities: Vec<String>,
}

/// Discover available Ollama models by querying the Ollama API
///
/// # Arguments
/// * `base_url` - The base URL of the Ollama API (e.g., "http://localhost:11434")
///
/// # Returns
/// A vector of tuples containing (model_identifier, display_name, supports_vision, supports_thinking)
///
/// # Errors
/// Returns an error if:
/// - The HTTP request fails
/// - The API returns a non-success status
/// - The response cannot be deserialized
pub async fn discover_ollama_models(base_url: &str) -> Result<Vec<(String, String, bool, bool)>> {
    // Build the API endpoint URL
    let url = format!("{}/api/tags", base_url.trim_end_matches('/'));

    // Make HTTP request to Ollama API
    let response = reqwest::get(&url).await?;

    if !response.status().is_success() {
        return Err(anyhow::anyhow!(
            "Ollama API returned status: {}",
            response.status()
        ));
    }

    let tags_response: OllamaTagsResponse = response.json().await?;

    let client = crate::services::http_client::default_client(30);
    let mut models = Vec::new();

    for m in tags_response.models {
        let identifier = m.name.clone();
        // Create a friendly display name:
        //   "qwen:3b"               → "Qwen 3b"
        //   "llama3.2-vision:latest" → "Llama3.2 Vision"
        //   "mistral:latest"         → "Mistral"
        let (base, tag) = identifier.split_once(':').unwrap_or((&identifier, ""));
        let base_name = base
            .split('-')
            .map(|s| {
                let mut c = s.chars();
                match c.next() {
                    None => String::new(),
                    Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                }
            })
            .collect::<Vec<_>>()
            .join(" ");
        let display_name = if tag.is_empty() || tag == "latest" {
            base_name
        } else {
            format!("{} {}", base_name, tag)
        };

        // Query /api/show to check for vision and thinking capabilities
        let (supports_vision, supports_thinking) =
            check_model_capabilities(&client, base_url, &identifier).await;

        models.push((identifier, display_name, supports_vision, supports_thinking));
    }

    Ok(models)
}

/// Check Ollama model capabilities (vision and thinking) by querying /api/show
async fn check_model_capabilities(
    client: &reqwest::Client,
    base_url: &str,
    model_name: &str,
) -> (bool, bool) {
    let url = format!("{}/api/show", base_url.trim_end_matches('/'));

    let response = client
        .post(&url)
        .json(&serde_json::json!({ "model": model_name }))
        .send()
        .await;

    match response {
        Ok(resp) if resp.status().is_success() => match resp.json::<OllamaShowResponse>().await {
            Ok(show) => {
                let has_vision = show.capabilities.iter().any(|c| c == "vision");
                let has_thinking = show.capabilities.iter().any(|c| c == "thinking");
                if has_vision {
                    debug!(model = %model_name, "Ollama model supports vision");
                }
                if has_thinking {
                    debug!(model = %model_name, "Ollama model supports thinking");
                }
                (has_vision, has_thinking)
            }
            Err(e) => {
                warn!(model = %model_name, error = ?e, "Failed to parse /api/show response");
                (false, false)
            }
        },
        Ok(resp) => {
            warn!(model = %model_name, status = %resp.status(), "Ollama /api/show returned error");
            (false, false)
        }
        Err(e) => {
            warn!(model = %model_name, error = ?e, "Failed to query Ollama /api/show");
            (false, false)
        }
    }
}
