use anyhow::Result;
use gpui::{App, AsyncApp, BorrowAppContext};
use tracing::{debug, error, info, warn};

use crate::MODELS_REPOSITORY;
use crate::settings::models::models_store::{ModelConfig, ModelsModel};
use crate::settings::models::providers_store::{ProviderModel, ProviderType};

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

            // Create ModelConfig for each discovered model
            let new_model_configs: Vec<ModelConfig> = discovered_models
                .iter()
                .map(|(identifier, display_name)| {
                    let id = format!("ollama-{}", identifier.replace(':', "-"));
                    ModelConfig::new(
                        id,
                        display_name.clone(),
                        ProviderType::Ollama,
                        identifier.clone(),
                    )
                })
                .collect();

            // Sync Ollama models: remove old ones, add new ones
            cx.update(|cx| {
                cx.update_global::<ModelsModel, _>(|model, _cx| {
                    // Get existing Ollama model IDs
                    let existing_ollama_ids: Vec<String> = model
                        .models_by_provider(&ProviderType::Ollama)
                        .iter()
                        .map(|m| m.id.clone())
                        .collect();

                    // Remove all existing Ollama models
                    for id in existing_ollama_ids {
                        model.delete_model(&id);
                    }

                    // Add newly discovered models
                    for config in &new_model_configs {
                        model.add_model(config.clone());
                    }

                    debug!(count = new_model_configs.len(), "Models synced");
                });

                // Refresh windows to update UI
                cx.refresh_windows();
            })?;

            // Save to disk
            let all_models = cx
                .update(|cx| cx.global::<ModelsModel>().models().to_vec())
                .ok();

            if let Some(all_models) = all_models {
                let models_repo = MODELS_REPOSITORY.clone();
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

            // Remove any existing Ollama models since none are available
            cx.update(|cx| {
                cx.update_global::<ModelsModel, _>(|model, _cx| {
                    let existing_ollama_ids: Vec<String> = model
                        .models_by_provider(&ProviderType::Ollama)
                        .iter()
                        .map(|m| m.id.clone())
                        .collect();

                    for id in existing_ollama_ids {
                        model.delete_model(&id);
                    }
                });
            })?;

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
