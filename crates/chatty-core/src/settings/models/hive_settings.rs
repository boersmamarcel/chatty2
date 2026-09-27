use chrono::{DateTime, Utc};
use hive_client::TokenPair;
use serde::{Deserialize, Serialize};

pub const DEFAULT_REGISTRY_URL: &str = "http://localhost:8080";
pub const DEFAULT_RUNNER_URL: &str = "http://localhost:8081";

/// Settings for the Hive module registry connection and account.
///
/// `Debug` is redacted: the tokens may never reach a log.
#[derive(Clone, Serialize, Deserialize)]
pub struct HiveSettingsModel {
    /// Base URL of the Hive registry.
    #[serde(default = "default_registry_url")]
    pub registry_url: String,
    /// Base URL of the Hive runner for remote module execution.
    #[serde(default = "default_runner_url")]
    pub runner_url: String,
    /// One-hour access JWT obtained via login/register, rotated by refresh.
    #[serde(default)]
    pub token: Option<String>,
    /// Opaque 30-day refresh token paired with `token`; rotates on every
    /// refresh, so only the current one is valid.
    #[serde(default)]
    pub refresh_token: Option<String>,
    /// When `token` expires.
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
    /// Cached username for display in the UI.
    #[serde(default)]
    pub username: Option<String>,
    /// Cached email for re-login flows.
    #[serde(default)]
    pub email: Option<String>,
}

fn default_registry_url() -> String {
    DEFAULT_REGISTRY_URL.to_string()
}

fn default_runner_url() -> String {
    DEFAULT_RUNNER_URL.to_string()
}

impl Default for HiveSettingsModel {
    fn default() -> Self {
        Self {
            registry_url: default_registry_url(),
            runner_url: default_runner_url(),
            token: None,
            refresh_token: None,
            expires_at: None,
            username: None,
            email: None,
        }
    }
}

impl std::fmt::Debug for HiveSettingsModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let redact = |t: &Option<String>| t.as_ref().map(|_| "<redacted>");
        f.debug_struct("HiveSettingsModel")
            .field("registry_url", &self.registry_url)
            .field("runner_url", &self.runner_url)
            .field("token", &redact(&self.token))
            .field("refresh_token", &redact(&self.refresh_token))
            .field("expires_at", &self.expires_at)
            .field("username", &self.username)
            .field("email", &self.email)
            .finish()
    }
}

impl HiveSettingsModel {
    pub fn is_logged_in(&self) -> bool {
        self.token.is_some()
    }

    /// The persisted token pair, when all three halves are present.
    pub fn token_pair(&self) -> Option<TokenPair> {
        Some(TokenPair {
            token: self.token.clone()?,
            refresh_token: self.refresh_token.clone()?,
            expires_at: self.expires_at?,
        })
    }

    /// Store `pair` (or clear it) as the persisted token pair.
    pub fn set_token_pair(&mut self, pair: Option<&TokenPair>) {
        self.token = pair.map(|p| p.token.clone());
        self.refresh_token = pair.map(|p| p.refresh_token.clone());
        self.expires_at = pair.map(|p| p.expires_at);
    }
}
