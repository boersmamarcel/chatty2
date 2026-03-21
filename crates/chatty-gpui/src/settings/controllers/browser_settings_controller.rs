use crate::settings::models::{AgentConfigEvent, GlobalAgentConfigNotifier};
use chatty_browser::settings::BrowserSettingsModel;
use gpui::{App, AsyncApp};
use tracing::{debug, error, info};

/// Emit `RebuildRequired` so the active conversation's agent is rebuilt
/// with the current browser tool settings.
fn notify_tool_set_changed(cx: &mut App) {
    if let Some(weak_notifier) = cx
        .try_global::<GlobalAgentConfigNotifier>()
        .and_then(|g| g.entity.clone())
        && let Some(notifier) = weak_notifier.upgrade()
    {
        info!("Notifying browser settings changed — triggering agent rebuild");
        notifier.update(cx, |_notifier, cx| {
            cx.emit(AgentConfigEvent::RebuildRequired);
        });
    } else {
        debug!(
            "notify_tool_set_changed: GlobalAgentConfigNotifier not found — agent will not be rebuilt"
        );
    }
}

/// Save browser settings asynchronously
fn save_async(cx: &mut App) {
    let settings = cx.global::<BrowserSettingsModel>().clone();
    cx.spawn(|_cx: &mut AsyncApp| async move {
        let repo = chatty_core::browser_settings_repository();
        if let Err(e) = repo.save(settings).await {
            error!(error = ?e, "Failed to save browser settings");
        }
    })
    .detach();
}

/// Toggle browser engine enabled/disabled and persist to disk
pub fn toggle_browser(cx: &mut App) {
    let new_enabled = !cx.global::<BrowserSettingsModel>().enabled;
    info!(new = new_enabled, "Toggling browser engine");
    cx.global_mut::<BrowserSettingsModel>().enabled = new_enabled;

    cx.refresh_windows();
    notify_tool_set_changed(cx);
    save_async(cx);
}

/// Toggle headless mode and persist to disk
pub fn toggle_headless(cx: &mut App) {
    let new_headless = !cx.global::<BrowserSettingsModel>().headless;
    info!(new = new_headless, "Toggling headless mode");
    cx.global_mut::<BrowserSettingsModel>().headless = new_headless;

    cx.refresh_windows();
    save_async(cx);
}

/// Toggle auth approval requirement and persist to disk
pub fn toggle_auth_approval(cx: &mut App) {
    let new_val = !cx.global::<BrowserSettingsModel>().require_auth_approval;
    info!(new = new_val, "Toggling browser auth approval");
    cx.global_mut::<BrowserSettingsModel>()
        .require_auth_approval = new_val;

    cx.refresh_windows();
    save_async(cx);
}

/// Toggle action approval requirement and persist to disk
pub fn toggle_action_approval(cx: &mut App) {
    let new_val = !cx.global::<BrowserSettingsModel>().require_action_approval;
    info!(new = new_val, "Toggling browser action approval");
    cx.global_mut::<BrowserSettingsModel>()
        .require_action_approval = new_val;

    cx.refresh_windows();
    save_async(cx);
}

/// Set the maximum number of concurrent tabs and persist to disk
pub fn set_max_tabs(count: u32, cx: &mut App) {
    let count = count.clamp(1, 20);
    info!(count, "Setting max browser tabs");
    cx.global_mut::<BrowserSettingsModel>().max_tabs = count;

    cx.refresh_windows();
    save_async(cx);
}

/// Set the page load timeout and persist to disk
pub fn set_timeout(seconds: u32, cx: &mut App) {
    let seconds = seconds.clamp(5, 120);
    info!(seconds, "Setting browser timeout");
    cx.global_mut::<BrowserSettingsModel>().timeout_seconds = seconds;

    cx.refresh_windows();
    save_async(cx);
}
