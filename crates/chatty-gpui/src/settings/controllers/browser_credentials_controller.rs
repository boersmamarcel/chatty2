use crate::settings::models::browser_credentials_store::{
    AuthType, BrowserCredentialsModel, CapturedCookie, WebCredential,
};
use crate::settings::models::{AgentConfigEvent, GlobalAgentConfigNotifier};
use gpui::{App, AsyncApp};
use tracing::{error, info, warn};

/// Emit `RebuildRequired` so the active conversation's agent is rebuilt
/// with the updated credentials available to the browser_auth tool.
fn notify_credentials_changed(cx: &mut App) {
    if let Some(weak_notifier) = cx
        .try_global::<GlobalAgentConfigNotifier>()
        .and_then(|g| g.entity.clone())
        && let Some(notifier) = weak_notifier.upgrade()
    {
        info!("Notifying browser credentials changed — triggering agent rebuild");
        notifier.update(cx, |_notifier, cx| {
            cx.emit(AgentConfigEvent::RebuildRequired);
        });
    } else {
        warn!(
            "notify_credentials_changed: GlobalAgentConfigNotifier not found — agent will not be rebuilt"
        );
    }
}

/// Save the current credentials model to disk asynchronously.
fn save_credentials_async(cx: &mut App) {
    let model = cx.global::<BrowserCredentialsModel>().clone();
    cx.spawn(|_cx: &mut AsyncApp| async move {
        let repo = chatty_core::browser_credentials_repository();
        if let Err(e) = repo.save(model).await {
            error!(error = ?e, "Failed to save browser credentials");
        }
    })
    .detach();
}

/// Add or update a browser credential, refresh UI, trigger agent rebuild, and save.
pub fn add_credential(credential: WebCredential, cx: &mut App) {
    info!(name = %credential.name, "Adding browser credential");

    let model = cx.global_mut::<BrowserCredentialsModel>();
    model.upsert(credential);

    cx.refresh_windows();
    notify_credentials_changed(cx);
    save_credentials_async(cx);
}

/// Remove a browser credential by name.
pub fn remove_credential(name: &str, cx: &mut App) {
    info!(name = %name, "Removing browser credential");

    let model = cx.global_mut::<BrowserCredentialsModel>();
    model.remove(name);

    cx.refresh_windows();
    notify_credentials_changed(cx);
    save_credentials_async(cx);
}

/// Store cookies captured from a browser session as a new credential.
pub fn store_captured_session(
    name: String,
    domain: String,
    cookies: Vec<(String, String)>,
    cx: &mut App,
) {
    let captured_cookies: Vec<CapturedCookie> = cookies
        .into_iter()
        .map(|(cookie_name, cookie_value)| CapturedCookie {
            name: cookie_name,
            value: cookie_value,
            domain: format!(".{}", domain),
            path: "/".to_string(),
        })
        .collect();

    let credential = WebCredential {
        name,
        url_pattern: format!("https://{}/*", domain),
        auth_type: AuthType::CapturedSession {
            cookies: captured_cookies,
            captured_at: chrono::Utc::now().to_rfc3339(),
        },
    };

    add_credential(credential, cx);
}
