use crate::settings::models::models_store::{ModelConfig, ModelsModel};
use crate::settings::models::{GlobalModelsNotifier, ModelsNotifierEvent};
use gpui::{App, AsyncApp};
use tracing::{error, info, warn};

/// Emit `ModelsChanged` so the main window chat-input model picker refreshes.
fn notify_models_changed(cx: &mut App) {
    if let Some(notifier) = cx
        .try_global::<GlobalModelsNotifier>()
        .and_then(|g| g.get())
    {
        info!("Notifying models changed — refreshing chat input picker");
        notifier.update(cx, |_notifier, cx| {
            cx.emit(ModelsNotifierEvent::ModelsChanged);
        });
    } else {
        warn!(
            "notify_models_changed: GlobalModelsNotifier not found — chat input will not refresh"
        );
    }
}

/// Publish a mutation of the models store: refresh the UI right away, then
/// write the new roster to disk in the background (the optimistic-update
/// pattern every mutation here follows).
fn commit(cx: &mut App) {
    let models_to_save = cx.global::<ModelsModel>().models().to_vec();

    cx.refresh_windows();
    notify_models_changed(cx);

    save_models_async(models_to_save, cx);
}

/// Create a new model
pub fn create_model(mut config: ModelConfig, cx: &mut App) {
    // Auto-set capabilities based on provider type
    let (supports_images, supports_pdf) = config.provider_type.default_capabilities();
    config.supports_images = supports_images;
    config.supports_pdf = supports_pdf;

    cx.global_mut::<ModelsModel>().add_model(config);

    commit(cx);
}

/// Toggle a model's favourite flag.
pub fn toggle_favorite(model_id: &str, cx: &mut App) {
    if cx
        .global_mut::<ModelsModel>()
        .toggle_favorite(model_id)
        .is_none()
    {
        error!(model_id, "Failed to toggle favourite: model not found");
        return;
    }

    commit(cx);
}

/// Make a model the default for new conversations, clearing the previous one.
pub fn set_default_model(model_id: &str, cx: &mut App) {
    if !cx.global_mut::<ModelsModel>().set_default(model_id) {
        error!(model_id, "Failed to set default: model not found");
        return;
    }

    commit(cx);
}

/// Update an existing model
pub fn update_model(updated_config: ModelConfig, cx: &mut App) {
    if !cx.global_mut::<ModelsModel>().update_model(updated_config) {
        error!("Failed to update model: model not found");
        return;
    }

    commit(cx);
}

/// Delete a model by ID
pub fn delete_model(model_id: String, cx: &mut App) {
    if !cx.global_mut::<ModelsModel>().delete_model(&model_id) {
        error!("Failed to delete model: model not found");
        return;
    }

    commit(cx);
}

/// Save models asynchronously to disk
fn save_models_async(models: Vec<ModelConfig>, cx: &mut App) {
    cx.spawn(|_cx: &mut AsyncApp| async move {
        let repo = chatty_core::models_repository();
        if let Err(e) = repo.save_all(models).await {
            error!(error = ?e, "Failed to save models, changes will be lost on restart");
        }
    })
    .detach();
}
