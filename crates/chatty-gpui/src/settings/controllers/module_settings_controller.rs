use crate::settings::models::module_settings::ModuleSettingsModel;
use gpui::{App, AsyncApp};
use tracing::{error, info};

/// Persist module settings asynchronously.
fn save_async(cx: &mut App) {
    let settings = cx.global::<ModuleSettingsModel>().clone();
    cx.spawn(|_cx: &mut AsyncApp| async move {
        let repo = chatty_core::module_settings_repository();
        if let Err(e) = repo.save(settings).await {
            error!(error = ?e, "Failed to save module settings");
        }
    })
    .detach();
}

/// Toggle the module runtime on/off.
pub fn toggle_enabled(cx: &mut App) {
    let new_val = !cx.global::<ModuleSettingsModel>().enabled;
    info!(enabled = new_val, "Toggling module runtime");
    cx.global_mut::<ModuleSettingsModel>().enabled = new_val;
    cx.refresh_windows();
    save_async(cx);
}

/// Update the module directory path.
pub fn set_module_dir(dir: String, cx: &mut App) {
    info!(dir = %dir, "Setting module directory");
    cx.global_mut::<ModuleSettingsModel>().module_dir = dir;
    cx.refresh_windows();
    save_async(cx);
}

/// Update the gateway port.
pub fn set_gateway_port(port: u16, cx: &mut App) {
    info!(port, "Setting gateway port");
    cx.global_mut::<ModuleSettingsModel>().gateway_port = port;
    cx.refresh_windows();
    save_async(cx);
}
