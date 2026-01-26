use anyhow::Result;
use serde::{Deserialize, Serialize};

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

/// Discover available Ollama models by querying the Ollama API
///
/// # Arguments
/// * `base_url` - The base URL of the Ollama API (e.g., "http://localhost:11434")
///
/// # Returns
/// A vector of tuples containing (model_identifier, display_name)
///
/// # Errors
/// Returns an error if:
/// - The HTTP request fails
/// - The API returns a non-success status
/// - The response cannot be deserialized
pub async fn discover_ollama_models(base_url: &str) -> Result<Vec<(String, String)>> {
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

    // Extract model names and create display names
    let models: Vec<(String, String)> = tags_response
        .models
        .into_iter()
        .map(|m| {
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

            (identifier, display_name)
        })
        .collect();

    Ok(models)
}
