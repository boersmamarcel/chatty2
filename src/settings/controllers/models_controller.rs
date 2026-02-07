use crate::settings::models::models_store::{ModelConfig, ModelsModel};
use gpui::{App, AsyncApp};
use tracing::error;

/// Create a new model
pub fn create_model(mut config: ModelConfig, cx: &mut App) {
    // Auto-set capabilities based on provider type
    let (supports_images, supports_pdf) = config.provider_type.default_capabilities();
    config.supports_images = supports_images;
    config.supports_pdf = supports_pdf;

    // 1. Apply update immediately (optimistic update)
    let model = cx.global_mut::<ModelsModel>();
    model.add_model(config);

    // 2. Get updated state for async save
    let models_to_save = cx.global::<ModelsModel>().models().to_vec();

    // 3. Refresh UI immediately (optimistic update)
    cx.refresh_windows();

    // 4. Save async with error handling
    save_models_async(models_to_save, cx);
}

/// Update an existing model
pub fn update_model(updated_config: ModelConfig, cx: &mut App) {
    // 1. Apply update immediately (optimistic update)
    let model = cx.global_mut::<ModelsModel>();

    if !model.update_model(updated_config) {
        error!("Failed to update model: model not found");
        return;
    }

    // 2. Get updated state for async save
    let models_to_save = cx.global::<ModelsModel>().models().to_vec();

    // 3. Refresh UI immediately (optimistic update)
    cx.refresh_windows();

    // 4. Save async with error handling
    save_models_async(models_to_save, cx);
}

/// Delete a model by ID
pub fn delete_model(model_id: String, cx: &mut App) {
    // 1. Apply update immediately (optimistic update)
    let model = cx.global_mut::<ModelsModel>();

    if !model.delete_model(&model_id) {
        error!("Failed to delete model: model not found");
        return;
    }

    // 2. Get updated state for async save
    let models_to_save = cx.global::<ModelsModel>().models().to_vec();

    // 3. Refresh UI immediately (optimistic update)
    cx.refresh_windows();

    // 4. Save async with error handling
    save_models_async(models_to_save, cx);
}

/// Save models asynchronously to disk
fn save_models_async(models: Vec<ModelConfig>, cx: &mut App) {
    use crate::MODELS_REPOSITORY;

    cx.spawn(|_cx: &mut AsyncApp| async move {
        let repo = MODELS_REPOSITORY.clone();
        if let Err(e) = repo.save_all(models).await {
            error!(error = ?e, "Failed to save models, changes will be lost on restart");
        }
    })
    .detach();
}
