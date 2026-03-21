use crate::settings::models::{AgentConfigEvent, GlobalAgentConfigNotifier};
use chatty_browser::credential::types::{AuthMethod, LoginProfile};
use chatty_browser::settings::BrowserCredentialsModel;
use chatty_browser::settings::browser_credentials::{
    new_form_login_profile, new_session_capture_profile,
};
use gpui::{App, AsyncApp};
use tracing::{error, info, warn};

/// Emit `RebuildRequired` so the active conversation's agent is rebuilt
/// with the updated login profiles available to browser_auth.
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
    let profiles = cx.global::<BrowserCredentialsModel>().profiles.clone();
    cx.spawn(|_cx: &mut AsyncApp| async move {
        match chatty_browser::settings::LoginProfileRepository::new() {
            Ok(repo) => {
                if let Err(e) = repo.save_all(&profiles).await {
                    error!(error = ?e, "Failed to save browser credentials");
                }
            }
            Err(e) => error!(error = ?e, "Failed to create LoginProfileRepository"),
        }
    })
    .detach();
}

/// Add a new form-login credential.
pub fn add_form_login(
    name: String,
    url_pattern: String,
    username_selector: String,
    password_selector: String,
    submit_selector: String,
    cx: &mut App,
) {
    info!(name = %name, url = %url_pattern, "Adding form-login credential");

    let profile = new_form_login_profile(
        name,
        url_pattern,
        username_selector,
        password_selector,
        submit_selector,
    );

    let model = cx.global_mut::<BrowserCredentialsModel>();
    model.upsert(profile);

    cx.refresh_windows();
    notify_credentials_changed(cx);
    save_credentials_async(cx);
}

/// Add a new session-capture credential.
pub fn add_session_capture(name: String, url_pattern: String, cx: &mut App) {
    info!(name = %name, url = %url_pattern, "Adding session-capture credential");

    let profile = new_session_capture_profile(name, url_pattern);

    let model = cx.global_mut::<BrowserCredentialsModel>();
    model.upsert(profile);

    cx.refresh_windows();
    notify_credentials_changed(cx);
    save_credentials_async(cx);
}

/// Remove a credential by name.
pub fn remove_credential(name: &str, cx: &mut App) {
    info!(name = %name, "Removing browser credential");

    let model = cx.global_mut::<BrowserCredentialsModel>();
    model.remove(name);

    cx.refresh_windows();
    notify_credentials_changed(cx);
    save_credentials_async(cx);

    // Also try to delete the secret from the vault
    let name_owned = name.to_string();
    cx.spawn(|_cx: &mut AsyncApp| async move {
        match chatty_browser::credential::vault::CredentialVault::new() {
            Ok(vault) => {
                if let Err(e) = vault.delete(&name_owned).await {
                    warn!(name = %name_owned, error = ?e, "Failed to delete credential from vault");
                }
            }
            Err(e) => warn!(error = ?e, "Failed to create CredentialVault for deletion"),
        }
    })
    .detach();
}

/// Store form credentials (username/password) in the OS keyring.
pub fn store_form_credentials(name: String, username: String, password: String, cx: &mut App) {
    info!(name = %name, "Storing form credentials in vault");

    // Optimistically mark as having a secret in the model
    let model = cx.global_mut::<BrowserCredentialsModel>();
    model.set_has_secret(&name, true);

    cx.spawn(|_cx: &mut AsyncApp| async move {
        match chatty_browser::credential::vault::CredentialVault::new() {
            Ok(vault) => {
                let secret = chatty_browser::credential::types::LoginSecret::FormCredentials {
                    username,
                    password,
                };
                if let Err(e) = vault.store(&name, &secret).await {
                    error!(name = %name, error = ?e, "Failed to store form credentials");
                }
            }
            Err(e) => error!(error = ?e, "Failed to create CredentialVault"),
        }
    })
    .detach();
}
