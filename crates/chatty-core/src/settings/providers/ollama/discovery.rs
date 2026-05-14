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

/// A model discovered from a running Ollama instance.
#[derive(Debug, Clone)]
pub struct DiscoveredOllamaModel {
    /// Ollama model identifier (e.g. `"llama3.2-vision:latest"`)
    pub identifier: String,
    /// Human-readable display name (e.g. `"Llama3.2 Vision"`)
    pub display_name: String,
    /// Whether the model supports image inputs
    pub supports_vision: bool,
    /// Whether the model supports thinking/reasoning mode
    pub supports_thinking: bool,
}

/// Discover available Ollama models by querying the Ollama API
///
/// # Arguments
/// * `base_url` - The base URL of the Ollama API (e.g., "http://localhost:11434")
///
/// # Errors
/// Returns an error if:
/// - The HTTP request fails
/// - The API returns a non-success status
/// - The response cannot be deserialized
pub async fn discover_ollama_models(base_url: &str) -> Result<Vec<DiscoveredOllamaModel>> {
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
        let caps = check_model_capabilities(&client, base_url, &identifier).await;

        models.push(DiscoveredOllamaModel {
            identifier,
            display_name,
            supports_vision: caps.vision,
            supports_thinking: caps.thinking,
        });
    }

    Ok(models)
}

/// Capability flags returned by `/api/show`
struct ModelCapabilities {
    vision: bool,
    thinking: bool,
}

/// Check Ollama model capabilities (vision and thinking) by querying /api/show
async fn check_model_capabilities(
    client: &reqwest::Client,
    base_url: &str,
    model_name: &str,
) -> ModelCapabilities {
    let url = format!("{}/api/show", base_url.trim_end_matches('/'));

    let response = client
        .post(&url)
        .json(&serde_json::json!({ "model": model_name }))
        .send()
        .await;

    match response {
        Ok(resp) if resp.status().is_success() => match resp.json::<OllamaShowResponse>().await {
            Ok(show) => {
                let vision = show.capabilities.iter().any(|c| c == "vision");
                let thinking = show.capabilities.iter().any(|c| c == "thinking");
                if vision {
                    debug!(model = %model_name, "Ollama model supports vision");
                }
                if thinking {
                    debug!(model = %model_name, "Ollama model supports thinking");
                }
                ModelCapabilities { vision, thinking }
            }
            Err(e) => {
                warn!(model = %model_name, error = ?e, "Failed to parse /api/show response");
                ModelCapabilities {
                    vision: false,
                    thinking: false,
                }
            }
        },
        Ok(resp) => {
            warn!(model = %model_name, status = %resp.status(), "Ollama /api/show returned error");
            ModelCapabilities {
                vision: false,
                thinking: false,
            }
        }
        Err(e) => {
            warn!(model = %model_name, error = ?e, "Failed to query Ollama /api/show");
            ModelCapabilities {
                vision: false,
                thinking: false,
            }
        }
    }
}
