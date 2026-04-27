use serde::{Deserialize, Serialize};

pub const DEFAULT_REGISTRY_URL: &str = "http://localhost:8080";
pub const DEFAULT_RUNNER_URL: &str = "http://localhost:8081";

/// Settings for the Hive module registry connection and account.
#[derive(Clone, Serialize, Deserialize)]
pub struct HiveSettingsModel {
    /// Base URL of the Hive registry.
    #[serde(default = "default_registry_url")]
    pub registry_url: String,
    /// Base URL of the Hive runner for remote module execution.
    #[serde(default = "default_runner_url")]
    pub runner_url: String,
    /// JWT token obtained via login/register (30-day expiry).
    #[serde(default)]
    pub token: Option<String>,
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
            username: None,
            email: None,
        }
    }
}

impl HiveSettingsModel {
    pub fn is_logged_in(&self) -> bool {
        self.token.is_some()
    }
}
