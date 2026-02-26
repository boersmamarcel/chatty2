use crate::TRAINING_SETTINGS_REPOSITORY;
use crate::settings::models::training_settings::TrainingSettingsModel;
use gpui::{App, AsyncApp};
use tracing::{error, info};

/// Toggle ATIF auto-export enabled/disabled and persist to disk
pub fn toggle_atif_auto_export(cx: &mut App) {
    // 1. Apply update immediately (optimistic update)
    let old_enabled = cx.global::<TrainingSettingsModel>().atif_auto_export;
    let new_enabled = !old_enabled;
    info!(
        old = old_enabled,
        new = new_enabled,
        "Toggling ATIF auto-export"
    );
    cx.global_mut::<TrainingSettingsModel>().atif_auto_export = new_enabled;

    // 2. Get updated state for async save
    let settings = cx.global::<TrainingSettingsModel>().clone();

    // 3. Refresh UI immediately (optimistic update)
    cx.refresh_windows();

    // 4. Save async with error handling
    cx.spawn(|_cx: &mut AsyncApp| async move {
        let repo = TRAINING_SETTINGS_REPOSITORY.clone();
        if let Err(e) = repo.save(settings).await {
            error!(error = ?e, "Failed to save training settings");
        }
    })
    .detach();
}
