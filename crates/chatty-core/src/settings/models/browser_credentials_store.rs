use serde::{Deserialize, Serialize};

/// A single browser cookie captured during a manual login session.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CapturedCookie {
    /// Cookie name.
    pub name: String,
    /// Cookie value.
    pub value: String,
    /// Domain the cookie belongs to (e.g. ".example.com").
    pub domain: String,
    /// Cookie path (e.g. "/").
    pub path: String,
}

/// Authentication type for a stored browser credential.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum AuthType {
    /// Session captured via manual login in a visible browser window.
    CapturedSession {
        /// Cookies captured from the browser session.
        cookies: Vec<CapturedCookie>,
        /// ISO 8601 timestamp when the session was captured.
        captured_at: String,
    },
}

/// A stored browser credential that can be injected into browser sessions.
///
/// Credentials are captured by opening a visible Verso window, letting the
/// user log in manually (handling 2FA, CAPTCHAs, etc.), then extracting the
/// cookies once they click "Done — capture session".
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WebCredential {
    /// User-defined name for this credential (e.g., "komoot", "strava").
    pub name: String,
    /// URL pattern this credential applies to (e.g., "https://example.com/*").
    pub url_pattern: String,
    /// The authentication data.
    pub auth_type: AuthType,
}

/// Global store for browser credentials.
///
/// Credentials are persisted to `~/.config/chatty/browser_credentials.json`.
/// When the LLM agent calls `browser_auth { credential_name: "..." }`, the
/// stored cookies are injected into the browser session so the agent is
/// instantly "logged in" without ever seeing a password.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct BrowserCredentialsModel {
    #[serde(default)]
    pub credentials: Vec<WebCredential>,
}

impl BrowserCredentialsModel {
    /// Find a credential by name (case-insensitive).
    pub fn find_by_name(&self, name: &str) -> Option<&WebCredential> {
        self.credentials
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case(name))
    }

    /// Add or replace a credential.
    pub fn upsert(&mut self, credential: WebCredential) {
        if let Some(existing) = self
            .credentials
            .iter_mut()
            .find(|c| c.name.eq_ignore_ascii_case(&credential.name))
        {
            *existing = credential;
        } else {
            self.credentials.push(credential);
        }
    }

    /// Remove a credential by name (case-insensitive). Returns true if found.
    pub fn remove(&mut self, name: &str) -> bool {
        let len_before = self.credentials.len();
        self.credentials
            .retain(|c| !c.name.eq_ignore_ascii_case(name));
        self.credentials.len() < len_before
    }

    /// List all credential names.
    pub fn names(&self) -> Vec<String> {
        self.credentials.iter().map(|c| c.name.clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_credential(name: &str) -> WebCredential {
        WebCredential {
            name: name.to_string(),
            url_pattern: format!("https://{}.com/*", name),
            auth_type: AuthType::CapturedSession {
                cookies: vec![CapturedCookie {
                    name: "session".to_string(),
                    value: "abc123".to_string(),
                    domain: format!(".{}.com", name),
                    path: "/".to_string(),
                }],
                captured_at: "2026-03-21T08:00:00Z".to_string(),
            },
        }
    }

    #[test]
    fn test_find_by_name_case_insensitive() {
        let model = BrowserCredentialsModel {
            credentials: vec![sample_credential("komoot")],
        };
        assert!(model.find_by_name("komoot").is_some());
        assert!(model.find_by_name("Komoot").is_some());
        assert!(model.find_by_name("KOMOOT").is_some());
        assert!(model.find_by_name("strava").is_none());
    }

    #[test]
    fn test_upsert_adds_new() {
        let mut model = BrowserCredentialsModel::default();
        model.upsert(sample_credential("komoot"));
        assert_eq!(model.credentials.len(), 1);
    }

    #[test]
    fn test_upsert_replaces_existing() {
        let mut model = BrowserCredentialsModel {
            credentials: vec![sample_credential("komoot")],
        };
        let mut updated = sample_credential("komoot");
        updated.url_pattern = "https://new.komoot.com/*".to_string();
        model.upsert(updated);
        assert_eq!(model.credentials.len(), 1);
        assert_eq!(model.credentials[0].url_pattern, "https://new.komoot.com/*");
    }

    #[test]
    fn test_remove() {
        let mut model = BrowserCredentialsModel {
            credentials: vec![sample_credential("komoot"), sample_credential("strava")],
        };
        assert!(model.remove("komoot"));
        assert_eq!(model.credentials.len(), 1);
        assert_eq!(model.credentials[0].name, "strava");
        assert!(!model.remove("komoot"));
    }

    #[test]
    fn test_names() {
        let model = BrowserCredentialsModel {
            credentials: vec![sample_credential("komoot"), sample_credential("strava")],
        };
        let names = model.names();
        assert_eq!(names, vec!["komoot", "strava"]);
    }
}
