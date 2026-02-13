use crate::EXECUTION_SETTINGS_REPOSITORY;
use crate::settings::models::execution_settings::{ApprovalMode, ExecutionSettingsModel};
use gpui::{App, AsyncApp};
use tracing::error;

/// Toggle code execution enabled/disabled and persist to disk
pub fn toggle_execution(cx: &mut App) {
    // 1. Apply update immediately (optimistic update)
    let new_enabled = !cx.global::<ExecutionSettingsModel>().enabled;
    cx.global_mut::<ExecutionSettingsModel>().enabled = new_enabled;

    // 2. Get updated state for async save
    let settings = cx.global::<ExecutionSettingsModel>().clone();

    // 3. Refresh UI immediately (optimistic update)
    cx.refresh_windows();

    // 4. Save async with error handling
    cx.spawn(|_cx: &mut AsyncApp| async move {
        let repo = EXECUTION_SETTINGS_REPOSITORY.clone();
        if let Err(e) = repo.save(settings).await {
            error!(error = ?e, "Failed to save execution settings");
        }
    })
    .detach();
}

/// Update workspace directory and persist to disk
pub fn set_workspace_dir(dir: Option<String>, cx: &mut App) {
    // 1. Apply update immediately
    cx.global_mut::<ExecutionSettingsModel>().workspace_dir = dir;

    // 2. Get updated state for async save
    let settings = cx.global::<ExecutionSettingsModel>().clone();

    // 3. Refresh UI immediately
    cx.refresh_windows();

    // 4. Save async with error handling
    cx.spawn(|_cx: &mut AsyncApp| async move {
        let repo = EXECUTION_SETTINGS_REPOSITORY.clone();
        if let Err(e) = repo.save(settings).await {
            error!(error = ?e, "Failed to save execution settings");
        }
    })
    .detach();
}

/// Update approval mode and persist to disk
pub fn set_approval_mode(mode: ApprovalMode, cx: &mut App) {
    // 1. Apply update immediately
    cx.global_mut::<ExecutionSettingsModel>().approval_mode = mode;

    // 2. Get updated state for async save
    let settings = cx.global::<ExecutionSettingsModel>().clone();

    // 3. Refresh UI immediately
    cx.refresh_windows();

    // 4. Save async with error handling
    cx.spawn(|_cx: &mut AsyncApp| async move {
        let repo = EXECUTION_SETTINGS_REPOSITORY.clone();
        if let Err(e) = repo.save(settings).await {
            error!(error = ?e, "Failed to save execution settings");
        }
    })
    .detach();
}
