use anyhow::Result;
use gpui::{AsyncApp, BorrowAppContext, Global};
use tracing::{debug, info, warn};

use chatty_core::settings::providers::openrouter::discovery::{
    OpenRouterModel, discover_openrouter_models, model_completion_cost, model_prompt_cost,
    model_supports_images, model_supports_pdf,
};

use crate::settings::models::models_store::{ModelConfig, ModelsModel};
use crate::settings::models::providers_store::ProviderType;

use super::curated_models::{CuratedModel, load_curated_models};

/// In-memory cache of the full OpenRouter catalog (not just the curated list).
/// Filled on every sync so the Add-Model dialog can search against it.
#[derive(Clone)]
pub struct OpenRouterCatalog {
    pub models: Vec<OpenRouterModel>,
}

impl Global for OpenRouterCatalog {}

/// Synchronise curated OpenRouter models with the local store.
/// Also populates the global `OpenRouterCatalog` with the full fetched list.

/// Synchronise curated OpenRouter models with the local store.
///
/// This runs **every startup** (asynchronously) whenever an OpenRouter provider
/// with a non-empty API key is configured.  It:
///
/// 1. Fetches the full model catalog from `https://openrouter.ai/api/v1/models`.
/// 2. Matches fetched models against the curated list (hardcoded defaults or
///    user-overridden `openrouter_curated.json`).
/// 3. Removes **old OpenRouter models that are not in the curated list** — this
///    prevents permanently stale entries when OpenRouter retires a version.
/// 4. Adds or updates the curated models with live metadata (image/PDF support,
///    context length, pricing if present, temperature support, etc.).
/// 5. Persists the updated `ModelsModel` to disk.
///
/// Non-curated models that were added manually by the user or discovered by a
/// previous search are **left untouched**.
pub async fn sync_openrouter_models(cx: &mut AsyncApp) -> Result<usize> {
    info!("Starting OpenRouter curated-model sync");

    let curated = load_curated_models();
    let _curated_ids: std::collections::HashSet<&str> =
        curated.iter().map(|c| c.id.as_str()).collect();

    // -----------------------------------------------------------------
    // 1. Discover what OpenRouter currently advertises.
    // -----------------------------------------------------------------
    let discovered = match discover_openrouter_models().await {
        Ok(m) => m,
        Err(e) => {
            warn!(error = %e, "Failed to fetch OpenRouter model catalog");
            return Err(e);
        }
    };

    if discovered.is_empty() {
        warn!("OpenRouter returned an empty catalog; aborting sync");
        return Ok(0);
    }

    // Map curated id -> discovered model data.
    let discovered_lookup: std::collections::HashMap<&str, &OpenRouterModel> =
        discovered.iter().map(|m| (m.id.as_str(), m)).collect();

    // -----------------------------------------------------------------
    // 2. Build new / updated ModelConfig entries for curated list.
    // -----------------------------------------------------------------
    let mut new_configs: Vec<ModelConfig> = Vec::with_capacity(curated.len());
    for cm in &curated {
        let Some(or_model) = discovered_lookup.get(cm.id.as_str()) else {
            warn!(id = %cm.id, "Curated model not found in OpenRouter catalog, skipping");
            continue;
        };

        let config = build_model_config(cm, or_model);
        new_configs.push(config);
    }

    if new_configs.is_empty() {
        warn!("None of the curated models were found in the OpenRouter catalog");
        return Ok(0);
    }

    let synced_ids: std::collections::HashSet<&str> =
        new_configs.iter().map(|c| c.id.as_str()).collect();

    // -----------------------------------------------------------------
    // 3. Update global ModelsModel in-place.
    // -----------------------------------------------------------------
    cx.update(|cx| {
        cx.update_global::<ModelsModel, _>(|model, _cx| {
            // --- 3a. Remove stale OpenRouter models not in curated list ---
            let existing_openrouter_ids: Vec<String> = model
                .models_by_provider(&ProviderType::OpenRouter)
                .iter()
                .map(|m| m.id.clone())
                .filter(|id| !synced_ids.contains(id.as_str()))
                .collect();

            for id in existing_openrouter_ids {
                model.delete_model(&id);
            }

            // --- 3b. Upsert each curated model ---
            for config in &new_configs {
                if model.get_model(&config.id).is_some() {
                    model.update_model(config.clone());
                    debug!(id = %config.id, "Updated existing OpenRouter model");
                } else {
                    model.add_model(config.clone());
                    debug!(id = %config.id, "Added new OpenRouter model");
                }
            }
        });

        cx.refresh_windows();
    })?;

    // -----------------------------------------------------------------
    // 4. Store full catalog in a global so the Add-Model dialog can search it.
    // -----------------------------------------------------------------
    let catalog = OpenRouterCatalog {
        models: discovered.clone(),
    };
    cx.update(|cx| {
        cx.set_global(catalog);
    })?;

    // -----------------------------------------------------------------
    // 5. Persist to disk.
    // -----------------------------------------------------------------
    let all_models = cx
        .update(|cx| cx.global::<ModelsModel>().models().to_vec())
        .map_err(|e| warn!(error = ?e, "Failed to read ModelsModel after OpenRouter sync"))
        .ok();

    if let Some(all_models) = all_models {
        let repo = chatty_core::models_repository();
        if let Err(e) = repo.save_all(all_models).await {
            warn!(error = ?e, "Failed to save models after OpenRouter sync");
        } else {
            info!(
                count = new_configs.len(),
                "OpenRouter models synced and saved"
            );
        }
    }

    Ok(new_configs.len())
}

/// Construct a `ModelConfig` from a curated entry + live OpenRouter metadata.
fn build_model_config(cm: &CuratedModel, data: &OpenRouterModel) -> ModelConfig {
    let id = cm.id.replace('/', "-");

    let mut config = ModelConfig::new(id, cm.name.clone(), ProviderType::OpenRouter, cm.id.clone());

    // Image / PDF support discovered from live metadata
    config.supports_images = model_supports_images(data);
    config.supports_pdf = model_supports_pdf(data);

    // Temperature support: most models support it unless explicitly excluded.
    config.supports_temperature = !data.supported_parameters.is_empty();

    // Context window - use model-level context_length as the authoritative value
    config.max_context_window = Some(data.context_length as i32);

    // Pricing in USD per 1 000 000 tokens
    config.cost_per_million_input_tokens = model_prompt_cost(data);
    config.cost_per_million_output_tokens = model_completion_cost(data);

    config
}
