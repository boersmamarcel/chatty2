use gpui::Global;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::providers_store::ProviderType;

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
        }
    }
}

#[derive(Clone)]
pub struct ModelsModel {
    models: Vec<ModelConfig>,
}

impl Global for ModelsModel {}

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
}

impl Default for ModelsModel {
    fn default() -> Self {
        Self::new()
    }
}
