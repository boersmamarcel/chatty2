// Hardcoded + user-overridable curated list of top OpenRouter models.
//
// Format of the default list (sorted by popularity / capability):
//   (openrouter_id, display_name, override_cost_input, override_cost_output)
// If None / None is given for costs, they are populated from the live API.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tracing::info;

/// A single curated model entry.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct CuratedModel {
    pub id: String,
    pub name: String,
}

impl CuratedModel {
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
        }
    }
}

/// Default top-20 curated models baked into the binary.
pub fn default_curated_models() -> Vec<CuratedModel> {
    // Verified against https://openrouter.ai/api/v1/models. Every id here was
    // live when the list was last refreshed; `x-ai/grok-4` and `x-ai/grok-4-fast`
    // were removed because OpenRouter had delisted them and they failed at use.
    //
    // Vision matters here beyond attachments: the built-in browser tools return
    // screenshots as image content, so a text-only model cannot run the
    // self-review loop. Models marked `text` below are fine for everything else.
    vec![
        CuratedModel::new("anthropic/claude-opus-5", "Claude Opus 5"),
        CuratedModel::new("anthropic/claude-sonnet-5", "Claude Sonnet 5"),
        CuratedModel::new("openai/gpt-5.6-terra", "GPT-5.6 Terra"),
        CuratedModel::new("google/gemini-3.1-pro-preview", "Gemini 3.1 Pro Preview"),
        CuratedModel::new("anthropic/claude-sonnet-4.6", "Claude Sonnet 4.6"),
        CuratedModel::new("openai/gpt-5.6-sol", "GPT-5.6 Sol"),
        CuratedModel::new("x-ai/grok-4.6", "Grok 4.6"),
        CuratedModel::new("moonshotai/kimi-k3", "Kimi K3"),
        CuratedModel::new("google/gemini-3-flash-preview", "Gemini 3 Flash Preview"),
        CuratedModel::new("anthropic/claude-haiku-4.5", "Claude Haiku 4.5"),
        CuratedModel::new("x-ai/grok-4.20", "Grok 4.20"),
        CuratedModel::new("minimax/minimax-m3", "MiniMax M3"),
        CuratedModel::new("openai/gpt-5.5", "GPT-5.5"),
        CuratedModel::new("moonshotai/kimi-k2.7-code", "Kimi K2.7 Code"),
        CuratedModel::new("qwen/qwen3.6-plus", "Qwen 3.6 Plus"),
        CuratedModel::new("z-ai/glm-5", "GLM-5"),
        CuratedModel::new("deepseek/deepseek-v4-pro", "DeepSeek V4 Pro"),
        CuratedModel::new("mistralai/mistral-large-2512", "Mistral Large 3"),
        CuratedModel::new("meta-llama/llama-4-maverick", "Llama 4 Maverick"),
        CuratedModel::new("nvidia/nemotron-3-super-120b-a12b", "Nemotron 3 Super"),
    ]
}

/// Load the curated list, falling back to defaults when the user config is absent.
///
/// Looks for `<config_dir>/chatty/openrouter_curated.json` which expects the shape:
/// ```json
/// [
///   { "id": "moonshotai/kimi-k2.6", "name": "Kimi K2.6" },
///   ...
/// ]
/// ```
/// If the file does not exist or is malformed, the hardcoded list is returned.
pub fn load_curated_models() -> Vec<CuratedModel> {
    let path = openrouter_curated_json_path();
    if !path.exists() {
        return default_curated_models();
    }

    match std::fs::read_to_string(&path) {
        Ok(text) => match serde_json::from_str::<Vec<CuratedModel>>(&text) {
            Ok(list) => {
                info!(count = list.len(), "Loaded custom OpenRouter curated list");
                list
            }
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "Malformed openrouter_curated.json, using defaults");
                default_curated_models()
            }
        },
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "Cannot read openrouter_curated.json, using defaults");
            default_curated_models()
        }
    }
}

/// Path to the user-overridable curated list.
fn openrouter_curated_json_path() -> PathBuf {
    let base = dirs::config_dir().unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    base.join("chatty").join("openrouter_curated.json")
}

/// Write a given curated list to disk if the user wants to save custom overrides.
#[allow(dead_code)]
pub fn save_curated_models(list: &[CuratedModel]) -> anyhow::Result<()> {
    let path = openrouter_curated_json_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(list)?;
    std::fs::write(&path, text)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The curated list is the model picker's default content. A duplicate or a
    /// malformed id shows up as a broken entry the user cannot fix from the UI.
    #[test]
    fn curated_defaults_are_well_formed_and_unique() {
        let models = default_curated_models();
        assert!(!models.is_empty());

        let mut seen = std::collections::HashSet::new();
        for model in &models {
            assert!(
                model.id.contains('/'),
                "{} is not a provider/model id",
                model.id
            );
            assert!(!model.name.trim().is_empty(), "{} has no name", model.id);
            assert!(
                seen.insert(model.id.clone()),
                "{} is listed twice",
                model.id
            );
        }
    }

    /// These were delisted by OpenRouter and failed at point of use. Keep them
    /// out rather than rediscovering the failure through a user report.
    #[test]
    fn curated_defaults_exclude_delisted_models() {
        let ids: Vec<&str> = default_curated_models()
            .iter()
            .map(|m| m.id.clone())
            .map(|s| Box::leak(s.into_boxed_str()) as &str)
            .collect();
        for delisted in ["x-ai/grok-4", "x-ai/grok-4-fast"] {
            assert!(
                !ids.contains(&delisted),
                "{delisted} is delisted on OpenRouter and must not be curated"
            );
        }
    }
}
