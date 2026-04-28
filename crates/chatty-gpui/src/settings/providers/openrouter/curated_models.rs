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
    vec![
        CuratedModel::new("moonshotai/kimi-k2.6", "Kimi K2.6"),
        CuratedModel::new("anthropic/claude-sonnet-4.6", "Claude Sonnet 4.6"),
        CuratedModel::new("deepseek/deepseek-v4-pro", "DeepSeek V4 Pro"),
        CuratedModel::new("anthropic/claude-opus-4.7", "Claude Opus 4.7"),
        CuratedModel::new("google/gemini-3-flash-preview", "Gemini 3 Flash Preview"),
        CuratedModel::new("minimax/minimax-m2.7", "MiniMax M2.7"),
        CuratedModel::new("x-ai/grok-4-fast", "Grok 4 Fast"),
        CuratedModel::new("stepfun/step-3.5-flash", "Step 3.5 Flash"),
        CuratedModel::new("nvidia/nemotron-3-super-120b-a12b", "Nemotron 3 Super"),
        CuratedModel::new("openai/gpt-5.5-pro", "GPT-5.5 Pro"),
        CuratedModel::new("meta-llama/llama-4-maverick", "Llama 4 Maverick"),
        CuratedModel::new("qwen/qwen3.6-plus", "Qwen 3.6 Plus"),
        CuratedModel::new("openai/gpt-4o", "GPT-4o"),
        CuratedModel::new("google/gemini-2.5-pro", "Gemini 2.5 Pro"),
        CuratedModel::new("x-ai/grok-4", "Grok 4"),
        CuratedModel::new("deepseek/deepseek-chat", "DeepSeek V3"),
        CuratedModel::new("anthropic/claude-sonnet-4", "Claude Sonnet 4"),
        CuratedModel::new("anthropic/claude-opus-4", "Claude Opus 4"),
        CuratedModel::new("mistralai/mistral-large-2512", "Mistral Large 3"),
        CuratedModel::new("openai/gpt-5.5", "GPT-5.5"),
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
