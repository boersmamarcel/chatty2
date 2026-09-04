use anyhow::Result;
use gpui::{App, AsyncApp, BorrowAppContext};
use tracing::{debug, info, warn};

use crate::settings::models::models_store::{ModelConfig, ModelsModel};
use crate::settings::models::providers_store::ProviderType;
use crate::settings::models::{GlobalModelsNotifier, ModelsNotifierEvent};

use super::discovery::discover_ollama_models;

/// Synchronize Ollama models with the models store
///
/// This function:
/// 1. Discovers available models from Ollama
/// 2. Removes old Ollama models from the store
/// 3. Adds newly discovered models
/// 4. Saves the updated models to disk
///
/// # Arguments
/// * `ollama_base_url` - The base URL of the Ollama API
/// * `cx` - The async app context
///
/// # Returns
/// The number of models synchronized, or an error
pub async fn sync_ollama_models(ollama_base_url: &str, cx: &mut AsyncApp) -> Result<usize> {
    info!("Attempting Ollama model auto-discovery");

    match discover_ollama_models(ollama_base_url).await {
        Ok(discovered_models) if !discovered_models.is_empty() => {
            info!(count = discovered_models.len(), "Ollama models discovered");

            // Create ModelConfig for each discovered model, with vision capability
            let new_model_configs: Vec<ModelConfig> = discovered_models
                .iter()
                .map(|(identifier, display_name, supports_vision)| {
                    let id = format!("ollama-{}", identifier.replace(':', "-"));
                    let mut config = ModelConfig::new(
                        id,
                        display_name.clone(),
                        ProviderType::Ollama,
                        identifier.clone(),
                    );
                    config.supports_images = *supports_vision;
                    config.synced()
                })
                .collect();

            // Sync Ollama models: drop the ones this sync owns that are gone,
            // then upsert what's installed now. Models the user added by hand
            // are ModelSource::User and stay put.
            cx.update(|cx| {
                cx.update_global::<ModelsModel, _>(|model, _cx| {
                    let discovered_ids: std::collections::HashSet<&str> =
                        new_model_configs.iter().map(|c| c.id.as_str()).collect();

                    for id in model.stale_sync_ids(&ProviderType::Ollama, &discovered_ids) {
                        model.delete_model(&id);
                    }

                    // Upsert, preserving the user's favourite/default flags.
                    for config in &new_model_configs {
                        match model.get_model(&config.id) {
                            Some(existing) => {
                                let mut config = config.clone();
                                config.is_favorite = existing.is_favorite;
                                config.is_default = existing.is_default;
                                model.update_model(config);
                            }
                            None => model.add_model(config.clone()),
                        }
                    }

                    debug!(count = new_model_configs.len(), "Models synced");
                });

                // Refresh windows to update UI
                cx.refresh_windows();

                if let Some(notifier) = cx
                    .try_global::<GlobalModelsNotifier>()
                    .and_then(|g| g.get())
                {
                    notifier.update(cx, |_notifier, cx| {
                        cx.emit(ModelsNotifierEvent::ModelsChanged);
                    });
                }
            })?;

            // Save to disk
            let all_models = cx
                .update(|cx| cx.global::<ModelsModel>().models().to_vec())
                .map_err(|e| warn!(error = ?e, "Failed to save models after Ollama sync"))
                .ok();

            if let Some(all_models) = all_models {
                let models_repo = chatty_core::models_repository();
                if let Err(e) = models_repo.save_all(all_models).await {
                    warn!(error = ?e, "Failed to save discovered models");
                } else {
                    debug!("Models saved to disk");
                }
            }

            Ok(new_model_configs.len())
        }
        Ok(_) => {
            info!(url = %ollama_base_url, "No Ollama models installed, install with: ollama pull <model-name>");

            // Drop the models this sync put there, since none are installed
            // any more. User-added Ollama entries are left alone.
            let removed = cx.update(|cx| {
                let mut removed = 0usize;
                cx.update_global::<ModelsModel, _>(|model, _cx| {
                    let none = std::collections::HashSet::new();
                    for id in model.stale_sync_ids(&ProviderType::Ollama, &none) {
                        model.delete_model(&id);
                        removed += 1;
                    }
                });

                cx.refresh_windows();

                if let Some(notifier) = cx
                    .try_global::<GlobalModelsNotifier>()
                    .and_then(|g| g.get())
                {
                    notifier.update(cx, |_notifier, cx| {
                        cx.emit(ModelsNotifierEvent::ModelsChanged);
                    });
                }

                removed
            })?;

            // Persist the removal — without this the store and the file on
            // disk disagree until some other write happens to flush it.
            if removed > 0 {
                let all_models = cx.update(|cx| cx.global::<ModelsModel>().models().to_vec())?;
                if let Err(e) = chatty_core::models_repository().save_all(all_models).await {
                    warn!(error = ?e, "Failed to save models after removing Ollama models");
                }
            }

            Ok(0)
        }
        Err(e) => {
            warn!(url = %ollama_base_url, error = ?e, "Could not connect to Ollama, make sure Ollama is running or install from: https://ollama.ai");
            Err(e)
        }
    }
}

/// Ensure default Ollama provider exists
pub fn ensure_default_ollama_provider(cx: &mut App) -> bool {
    use crate::settings::models::providers_store::{ProviderConfig, ProviderModel, ProviderType};

    let mut should_save = false;

    cx.update_global::<ProviderModel, _>(|model, _cx| {
        // Check if Ollama provider exists
        if !model
            .providers()
            .iter()
            .any(|p| matches!(p.provider_type, ProviderType::Ollama))
        {
            let ollama_config = ProviderConfig::new("Ollama".to_string(), ProviderType::Ollama)
                .with_base_url("http://localhost:11434".to_string());
            model.add_provider(ollama_config);
            info!("Created default Ollama provider");
            should_save = true;
        }
    });

    should_save
}
