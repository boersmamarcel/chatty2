use crate::USER_SECRETS_REPOSITORY;
use crate::settings::models::user_secrets_store::{UserSecret, UserSecretsModel};
use crate::settings::models::{GlobalMcpNotifier, McpNotifierEvent};
use gpui::{App, AsyncApp};
use tracing::{error, info, warn};

/// Emit `ServersUpdated` so the active conversation's agent is rebuilt
/// with the updated user secrets injected into the shell session.
fn notify_secrets_changed(cx: &mut App) {
    if let Some(weak_notifier) = cx
        .try_global::<GlobalMcpNotifier>()
        .and_then(|g| g.entity.clone())
        && let Some(notifier) = weak_notifier.upgrade()
    {
        info!("Notifying secrets changed — triggering agent rebuild");
        notifier.update(cx, |_notifier, cx| {
            cx.emit(McpNotifierEvent::ServersUpdated);
        });
    } else {
        warn!("notify_secrets_changed: GlobalMcpNotifier not found — agent will not be rebuilt");
    }
}

/// Save the current secrets model to disk asynchronously.
fn save_secrets_async(cx: &mut App) {
    let model = cx.global::<UserSecretsModel>().clone();
    cx.spawn(|_cx: &mut AsyncApp| async move {
        let repo = USER_SECRETS_REPOSITORY.clone();
        if let Err(e) = repo.save(model).await {
            error!(error = ?e, "Failed to save user secrets");
        }
    })
    .detach();
}

/// Add or update a secret, refresh UI, trigger agent rebuild, and save to disk.
pub fn add_secret(key: String, value: String, cx: &mut App) {
    info!(key = %key, "Adding user secret");

    let model = cx.global_mut::<UserSecretsModel>();
    if let Some(existing) = model.secrets.iter_mut().find(|s| s.key == key) {
        existing.value = value;
    } else {
        model.secrets.push(UserSecret { key, value });
    }

    cx.refresh_windows();
    notify_secrets_changed(cx);
    save_secrets_async(cx);
}

/// Remove a secret by key.
pub fn remove_secret(key: &str, cx: &mut App) {
    info!(key = %key, "Removing user secret");

    let model = cx.global_mut::<UserSecretsModel>();
    model.secrets.retain(|s| s.key != key);
    model.revealed_keys.remove(key);

    cx.refresh_windows();
    notify_secrets_changed(cx);
    save_secrets_async(cx);
}

/// Toggle whether a secret's value is revealed in the settings UI.
pub fn toggle_revealed(key: &str, cx: &mut App) {
    let model = cx.global_mut::<UserSecretsModel>();
    if model.revealed_keys.contains(key) {
        model.revealed_keys.remove(key);
    } else {
        model.revealed_keys.insert(key.to_string());
    }
    cx.refresh_windows();
}
