use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::providers_store::ProviderType;

/// Default API version for Azure OpenAI
pub const AZURE_DEFAULT_API_VERSION: &str = "2025-03-01-preview";

/// Who put a model in the roster.
///
/// Provider sync services prune the models they manage so retired entries
/// don't linger, and this field is what keeps that pruning off everything
/// else. Anything the user added by hand is [`ModelSource::User`] and is
/// never removed by a sync.
///
/// Entries written before this field existed deserialize as `User` — the
/// safe direction, since treating a sync-managed model as user-owned only
/// leaves a stale row behind, while the reverse silently deletes the
/// user's own models.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelSource {
    /// Added by the user, by hand or from a catalogue pick.
    #[default]
    User,
    /// Created by a provider sync service, and owned by it.
    Sync,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModelConfig {
    pub id: String,
    pub name: String,
    pub provider_type: ProviderType,
    pub model_identifier: String,
    #[serde(default = "default_temperature")]
    pub temperature: f32,
    #[serde(default)]
    pub preamble: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub extra_params: HashMap<String, String>,
    /// Cost per million input tokens in USD (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_per_million_input_tokens: Option<f64>,
    /// Cost per million output tokens in USD (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_per_million_output_tokens: Option<f64>,
    /// Whether this model supports image inputs
    #[serde(default)]
    pub supports_images: bool,
    /// Whether this model supports PDF document inputs
    #[serde(default)]
    pub supports_pdf: bool,
    /// Whether this model supports the temperature parameter
    /// Some models (like OpenAI reasoning models) don't support temperature
    #[serde(default = "default_supports_temperature")]
    pub supports_temperature: bool,
    /// Max context window in tokens (used for the footer fill indicator)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_context_window: Option<i32>,
    /// Who owns this entry — see [`ModelSource`]
    #[serde(default)]
    pub source: ModelSource,
    /// Pinned to the top of the roster and the chat-input picker
    #[serde(default)]
    pub is_favorite: bool,
    /// The model new conversations start with. At most one model carries
    /// this; `models_controller::set_default_model` clears the others.
    #[serde(default)]
    pub is_default: bool,
}

fn default_temperature() -> f32 {
    1.0
}

fn default_supports_temperature() -> bool {
    true // Most models support temperature
}

impl ModelConfig {
    pub fn new(
        id: String,
        name: String,
        provider_type: ProviderType,
        model_identifier: String,
    ) -> Self {
        Self {
            id,
            name,
            provider_type,
            model_identifier,
            temperature: default_temperature(),
            preamble: String::new(),
            max_tokens: None,
            top_p: None,
            extra_params: HashMap::new(),
            cost_per_million_input_tokens: None,
            cost_per_million_output_tokens: None,
            supports_images: false,
            supports_pdf: false,
            supports_temperature: true,
            max_context_window: None,
            source: ModelSource::User,
            is_favorite: false,
            is_default: false,
        }
    }

    /// Mark this config as owned by a provider sync service.
    pub fn synced(mut self) -> Self {
        self.source = ModelSource::Sync;
        self
    }
}

#[derive(Clone)]
pub struct ModelsModel {
    models: Vec<ModelConfig>,
}

impl ModelsModel {
    pub fn new() -> Self {
        Self { models: Vec::new() }
    }

    pub fn add_model(&mut self, config: ModelConfig) {
        self.models.push(config);
    }

    pub fn update_model(&mut self, updated_config: ModelConfig) -> bool {
        if let Some(model) = self.models.iter_mut().find(|m| m.id == updated_config.id) {
            *model = updated_config;
            true
        } else {
            false
        }
    }

    pub fn delete_model(&mut self, id: &str) -> bool {
        let initial_len = self.models.len();
        self.models.retain(|m| m.id != id);
        self.models.len() < initial_len
    }

    pub fn get_model(&self, id: &str) -> Option<&ModelConfig> {
        self.models.iter().find(|m| m.id == id)
    }

    pub fn models(&self) -> &[ModelConfig] {
        &self.models
    }

    pub fn models_by_provider(&self, provider_type: &ProviderType) -> Vec<&ModelConfig> {
        self.models
            .iter()
            .filter(|m| &m.provider_type == provider_type)
            .collect()
    }

    /// Replace all models (used when loading from disk)
    pub fn replace_all(&mut self, models: Vec<ModelConfig>) {
        self.models = models;
    }

    /// IDs a provider sync should drop: its own [`ModelSource::Sync`] entries
    /// for `provider_type` that the provider no longer advertises.
    ///
    /// `keep` holds the IDs the sync just built. Everything absent from it is
    /// stale — but only if the sync created it. A model the user added by
    /// hand is never in `keep` (it isn't in the provider's curated list), so
    /// without the source check this is exactly the query that deletes the
    /// user's own models on every startup.
    pub fn stale_sync_ids(
        &self,
        provider_type: &ProviderType,
        keep: &std::collections::HashSet<&str>,
    ) -> Vec<String> {
        self.models
            .iter()
            .filter(|m| &m.provider_type == provider_type)
            .filter(|m| m.source == ModelSource::Sync)
            .filter(|m| !keep.contains(m.id.as_str()))
            .map(|m| m.id.clone())
            .collect()
    }

    /// Flip a model's favourite flag. Returns the new value, or `None` if no
    /// such model.
    pub fn toggle_favorite(&mut self, id: &str) -> Option<bool> {
        let model = self.models.iter_mut().find(|m| m.id == id)?;
        model.is_favorite = !model.is_favorite;
        Some(model.is_favorite)
    }

    /// Make `id` the default model, clearing the flag everywhere else so at
    /// most one model ever carries it. Returns false if no such model.
    pub fn set_default(&mut self, id: &str) -> bool {
        if !self.models.iter().any(|m| m.id == id) {
            return false;
        }
        for model in &mut self.models {
            model.is_default = model.id == id;
        }
        true
    }

    /// The model new conversations start with, if one is marked.
    pub fn default_model(&self) -> Option<&ModelConfig> {
        self.models.iter().find(|m| m.is_default)
    }
}

impl Default for ModelsModel {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn model(id: &str, provider: ProviderType, source: ModelSource) -> ModelConfig {
        let mut config = ModelConfig::new(
            id.to_string(),
            id.to_string(),
            provider,
            format!("vendor/{id}"),
        );
        config.source = source;
        config
    }

    fn store(models: Vec<ModelConfig>) -> ModelsModel {
        let mut store = ModelsModel::new();
        store.replace_all(models);
        store
    }

    /// The regression this whole field exists for: a hand-added model is
    /// never in the curated list, so the old "delete everything not in
    /// `keep`" query removed it on every startup and then saved the
    /// pruned roster over the user's file.
    #[test]
    fn user_added_models_survive_a_sync_that_does_not_list_them() {
        let store = store(vec![
            model("curated-a", ProviderType::OpenRouter, ModelSource::Sync),
            model("retired-b", ProviderType::OpenRouter, ModelSource::Sync),
            model("my-own", ProviderType::OpenRouter, ModelSource::User),
        ]);

        let keep = HashSet::from(["curated-a"]);
        let stale = store.stale_sync_ids(&ProviderType::OpenRouter, &keep);

        assert_eq!(stale, vec!["retired-b".to_string()]);
    }

    #[test]
    fn pruning_is_scoped_to_one_provider() {
        let store = store(vec![
            model("or-gone", ProviderType::OpenRouter, ModelSource::Sync),
            model("ollama-gone", ProviderType::Ollama, ModelSource::Sync),
        ]);

        let keep = HashSet::new();

        assert_eq!(
            store.stale_sync_ids(&ProviderType::Ollama, &keep),
            vec!["ollama-gone".to_string()]
        );
    }

    /// Entries written before `source` existed must read back as `User`,
    /// so the first launch after upgrading doesn't delete them.
    #[test]
    fn configs_without_a_source_field_deserialize_as_user_owned() {
        let json = r#"{
            "id": "legacy",
            "name": "Legacy",
            "provider_type": "open_router",
            "model_identifier": "vendor/legacy"
        }"#;

        let config: ModelConfig = serde_json::from_str(json).expect("legacy config parses");

        assert_eq!(config.source, ModelSource::User);
        assert!(!config.is_favorite);
        assert!(!config.is_default);
    }

    #[test]
    fn set_default_is_exclusive() {
        let mut store = store(vec![
            model("a", ProviderType::OpenRouter, ModelSource::User),
            model("b", ProviderType::OpenRouter, ModelSource::User),
        ]);

        assert!(store.set_default("a"));
        assert!(store.set_default("b"));

        assert_eq!(store.default_model().map(|m| m.id.as_str()), Some("b"));
        assert_eq!(store.models().iter().filter(|m| m.is_default).count(), 1);
    }

    #[test]
    fn set_default_rejects_an_unknown_id() {
        let mut store = store(vec![model(
            "a",
            ProviderType::OpenRouter,
            ModelSource::User,
        )]);

        assert!(!store.set_default("nope"));
        assert!(store.default_model().is_none());
    }

    #[test]
    fn toggle_favorite_flips_and_reports() {
        let mut store = store(vec![model(
            "a",
            ProviderType::OpenRouter,
            ModelSource::User,
        )]);

        assert_eq!(store.toggle_favorite("a"), Some(true));
        assert_eq!(store.toggle_favorite("a"), Some(false));
        assert_eq!(store.toggle_favorite("missing"), None);
    }
}
