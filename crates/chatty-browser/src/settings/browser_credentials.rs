//! In-memory model for browser login credentials.
//!
//! This is the GPUI-side model that holds the list of login profiles
//! currently loaded from disk. It follows the same pattern as
//! `UserSecretsModel` — the UI reads from this global, and the
//! controller mutates it + saves to disk asynchronously.

use std::collections::HashSet;

use crate::credential::types::{AuthMethod, LoginProfile};

/// Global model for browser login credentials displayed in Settings.
///
/// Holds non-sensitive profile metadata. Actual secrets (passwords,
/// cookies) are stored in [`CredentialVault`](crate::credential::vault)
/// and never appear in this model.
#[derive(Clone, Default)]
pub struct BrowserCredentialsModel {
    /// Login profiles loaded from `~/.config/chatty/login_profiles.json`.
    pub profiles: Vec<LoginProfile>,

    /// Profile names whose URL patterns are temporarily revealed in the UI
    /// (not really secret, but keeps the UI pattern consistent).
    #[allow(dead_code)]
    pub revealed_names: HashSet<String>,
}

impl BrowserCredentialsModel {
    /// Create a new empty model.
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace all profiles (used after loading from disk).
    pub fn replace_all(&mut self, profiles: Vec<LoginProfile>) {
        self.profiles = profiles;
    }

    /// Add or update a profile by name.
    pub fn upsert(&mut self, profile: LoginProfile) {
        if let Some(existing) = self.profiles.iter_mut().find(|p| p.name == profile.name) {
            *existing = profile;
        } else {
            self.profiles.push(profile);
        }
    }

    /// Remove a profile by name. Returns `true` if found and removed.
    pub fn remove(&mut self, name: &str) -> bool {
        let before = self.profiles.len();
        self.profiles.retain(|p| p.name != name);
        self.revealed_names.remove(name);
        self.profiles.len() < before
    }

    /// Return profile names suitable for the `browser_auth` tool.
    pub fn profile_names(&self) -> Vec<String> {
        self.profiles.iter().map(|p| p.name.clone()).collect()
    }
}

/// Convenience: create a new `LoginProfile` from dialog fields.
pub fn new_form_login_profile(
    name: String,
    url_pattern: String,
    username_selector: String,
    password_selector: String,
    submit_selector: String,
) -> LoginProfile {
    LoginProfile {
        name,
        url_pattern,
        auth_method: AuthMethod::FormLogin,
        username_selector: Some(username_selector),
        password_selector: Some(password_selector),
        submit_selector: if submit_selector.is_empty() {
            None
        } else {
            Some(submit_selector)
        },
        last_used: None,
        created_at: None,
    }
}

/// Convenience: create a new `SessionCapture` profile.
pub fn new_session_capture_profile(name: String, url_pattern: String) -> LoginProfile {
    LoginProfile {
        name,
        url_pattern,
        auth_method: AuthMethod::SessionCapture,
        username_selector: None,
        password_selector: None,
        submit_selector: None,
        last_used: None,
        created_at: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_model_upsert_adds_new() {
        let mut model = BrowserCredentialsModel::new();
        model.upsert(LoginProfile::new("github", "https://github.com"));
        assert_eq!(model.profiles.len(), 1);
        assert_eq!(model.profiles[0].name, "github");
    }

    #[test]
    fn test_model_upsert_updates_existing() {
        let mut model = BrowserCredentialsModel::new();
        model.upsert(LoginProfile::new("github", "https://github.com"));
        model.upsert(LoginProfile::new("github", "https://github.com/login"));
        assert_eq!(model.profiles.len(), 1);
        assert_eq!(model.profiles[0].url_pattern, "https://github.com/login");
    }

    #[test]
    fn test_model_remove() {
        let mut model = BrowserCredentialsModel::new();
        model.upsert(LoginProfile::new("github", "https://github.com"));
        assert!(model.remove("github"));
        assert!(model.profiles.is_empty());
        assert!(!model.remove("github"));
    }

    #[test]
    fn test_new_form_login_profile() {
        let p = new_form_login_profile(
            "test".into(),
            "https://example.com".into(),
            "#user".into(),
            "#pass".into(),
            "".into(),
        );
        assert_eq!(p.auth_method, AuthMethod::FormLogin);
        assert_eq!(p.username_selector.as_deref(), Some("#user"));
        assert!(p.submit_selector.is_none());
    }

    #[test]
    fn test_new_session_capture_profile() {
        let p = new_session_capture_profile("komoot".into(), "https://www.komoot.com".into());
        assert_eq!(p.auth_method, AuthMethod::SessionCapture);
        assert!(p.username_selector.is_none());
    }
}
