use crate::GENERAL_SETTINGS_REPOSITORY;
use crate::settings::models::GeneralSettingsModel;
use gpui::{App, AsyncApp};

/// Update font size and persist to disk
pub fn update_font_size(cx: &mut App, font_size: f32) {
    // 1. Apply update immediately (optimistic update)
    cx.global_mut::<GeneralSettingsModel>().font_size = font_size;

    // 2. Get updated state for async save
    let settings = cx.global::<GeneralSettingsModel>().clone();

    // 3. Refresh UI immediately (optimistic update)
    cx.refresh_windows();

    // 4. Save async with error handling
    cx.spawn(|_cx: &mut AsyncApp| async move {
        let repo = GENERAL_SETTINGS_REPOSITORY.clone();
        if let Err(e) = repo.save(settings).await {
            eprintln!("Failed to save general settings: {}", e);
            eprintln!("Changes will be lost on restart - please try again");
        }
    })
    .detach();
}

/// Update line height and persist to disk
pub fn update_line_height(cx: &mut App, line_height: f32) {
    // 1. Apply update immediately (optimistic update)
    cx.global_mut::<GeneralSettingsModel>().line_height = line_height;

    // 2. Get updated state for async save
    let settings = cx.global::<GeneralSettingsModel>().clone();

    // 3. Refresh UI immediately (optimistic update)
    cx.refresh_windows();

    // 4. Save async with error handling
    cx.spawn(|_cx: &mut AsyncApp| async move {
        let repo = GENERAL_SETTINGS_REPOSITORY.clone();
        if let Err(e) = repo.save(settings).await {
            eprintln!("Failed to save general settings: {}", e);
            eprintln!("Changes will be lost on restart - please try again");
        }
    })
    .detach();
}
