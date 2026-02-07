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
/// A vector of tuples containing (model_identifier, display_name, supports_vision)
///
/// # Errors
/// Returns an error if:
/// - The HTTP request fails
/// - The API returns a non-success status
/// - The response cannot be deserialized
pub async fn discover_ollama_models(base_url: &str) -> Result<Vec<(String, String, bool)>> {
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

    let client = reqwest::Client::new();
    let mut models = Vec::new();

    for m in tags_response.models {
        let identifier = m.name.clone();
        // Create a friendly display name (capitalize first letter, remove tags)
        let display_name = identifier
            .split(':')
            .next()
            .unwrap_or(&identifier)
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

        // Query /api/show to check for vision capability
        let supports_vision = check_model_vision(&client, base_url, &identifier).await;

        models.push((identifier, display_name, supports_vision));
    }

    Ok(models)
}

/// Check if an Ollama model supports vision by querying /api/show
async fn check_model_vision(client: &reqwest::Client, base_url: &str, model_name: &str) -> bool {
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
                if has_vision {
                    debug!(model = %model_name, "Ollama model supports vision");
                }
                has_vision
            }
            Err(e) => {
                warn!(model = %model_name, error = ?e, "Failed to parse /api/show response");
                false
            }
        },
        Ok(resp) => {
            warn!(model = %model_name, status = %resp.status(), "Ollama /api/show returned error");
            false
        }
        Err(e) => {
            warn!(model = %model_name, error = ?e, "Failed to query Ollama /api/show");
            false
        }
    }
}
