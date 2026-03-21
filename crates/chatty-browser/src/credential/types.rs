use serde::{Deserialize, Serialize};

// ── Auth method ─────────────────────────────────────────────────────────────

/// How a credential authenticates with a website.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum AuthMethod {
    /// Capture cookies from a manual login session (recommended for OAuth/2FA).
    #[default]
    SessionCapture,
    /// Automated form login using stored username/password.
    FormLogin,
}

impl std::fmt::Display for AuthMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SessionCapture => write!(f, "Session Capture"),
            Self::FormLogin => write!(f, "Form Login"),
        }
    }
}

// ── Login profile ───────────────────────────────────────────────────────────

/// Non-sensitive profile metadata for a stored credential.
///
/// Stored in `login_profiles.json` — contains no secrets. Secrets (passwords,
/// cookies) live in the OS keyring via [`CredentialVault`](crate::credential::vault).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LoginProfile {
    /// Unique name for this credential (e.g., "komoot", "strava").
    pub name: String,
    /// URL pattern to match (e.g., "https://www.komoot.com").
    pub url_pattern: String,
    /// Authentication method.
    pub auth_method: AuthMethod,

    // ── Form login fields (only used when auth_method == FormLogin) ──────
    /// CSS selector for the username field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username_selector: Option<String>,
    /// CSS selector for the password field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password_selector: Option<String>,
    /// CSS selector for the submit button.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submit_selector: Option<String>,

    // ── Metadata (stored as ISO 8601 strings for serde compatibility) ────
    /// When this credential was last used successfully (ISO 8601).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used: Option<String>,
    /// When this credential was created or last captured (ISO 8601).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
}

impl LoginProfile {
    pub fn new(name: impl Into<String>, url_pattern: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            url_pattern: url_pattern.into(),
            auth_method: AuthMethod::default(),
            username_selector: None,
            password_selector: None,
            submit_selector: None,
            last_used: None,
            created_at: None,
        }
    }
}

// ── Login secret ────────────────────────────────────────────────────────────

/// Secret material stored in the OS keyring.
///
/// Never exposed to the LLM, never serialized to disk. Loaded transiently
/// from the keyring and dropped after use.
#[derive(Clone, Debug)]
pub enum LoginSecret {
    /// Session cookies captured from a manual login.
    SessionCookies(Vec<crate::backend::Cookie>),
    /// Username/password pair for form login.
    FormCredentials { username: String, password: String },
}
