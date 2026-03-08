use gpui::Global;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// A single user-defined secret (environment variable).
///
/// Secrets are injected into shell sessions as environment variables
/// but are never exposed to the LLM.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UserSecret {
    pub key: String,
    pub value: String,
}

/// Global store for user-defined secrets.
///
/// Secrets are persisted to `~/.config/chatty/user_secrets.json` and
/// injected into every shell session as environment variables so that
/// scripts can access them via `os.environ["KEY"]` without the LLM
/// ever seeing the actual values.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct UserSecretsModel {
    #[serde(default)]
    pub secrets: Vec<UserSecret>,

    /// Keys whose values are temporarily revealed in the settings UI.
    #[serde(skip)]
    pub revealed_keys: HashSet<String>,
}

impl Global for UserSecretsModel {}

impl UserSecretsModel {
    /// Return secrets as (key, value) pairs for shell injection.
    pub fn as_env_pairs(&self) -> Vec<(String, String)> {
        self.secrets
            .iter()
            .map(|s| (s.key.clone(), s.value.clone()))
            .collect()
    }
}
