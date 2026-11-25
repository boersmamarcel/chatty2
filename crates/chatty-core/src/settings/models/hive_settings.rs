use chrono::{DateTime, Utc};
use hive_client::TokenPair;
use serde::{Deserialize, Serialize};

/// The Hive address a new install starts with: the local development
/// chatty-server. No production origin is configured yet; it is set here at
/// go-live (AGE-2085).
pub const DEFAULT_HIVE_URL: &str = "http://localhost:8081";

/// Settings for the Hive connection and account.
///
/// `Debug` is redacted: the tokens may never reach a log.
///
/// One address (`hive_url`, the chatty-server origin, which also proxies
/// `/api/auth/*`) serves sign-in and hosted conversations (AGE-2085). A file
/// written before that named the module registry as `registry_url`; that
/// loads as `registry_url_override`, so the marketplace and downloads keep
/// going where they did, and `hive_url` is its own value (or the default).
///
/// A file still carrying the removed `runner_url` (PL-H8b) fails to load with
/// an error naming it; any other unknown key is ignored as before.
#[derive(Clone, Serialize, Deserialize)]
#[serde(try_from = "HiveSettingsWire")]
pub struct HiveSettingsModel {
    /// The Hive server's address: sign-in, the account and hosted
    /// conversations all use it.
    #[serde(default = "default_hive_url")]
    pub hive_url: String,
    /// Developer setting: a different module registry for the marketplace
    /// and downloads. `None` (the normal case) uses `hive_url`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registry_url_override: Option<String>,
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

/// What a settings file holds: the model's fields plus the removed
/// `runner_url`, which only exists to be refused.
#[derive(Deserialize)]
struct HiveSettingsWire {
    #[serde(default)]
    hive_url: Option<String>,
    /// The pre-AGE-2085 module registry address; becomes the override.
    #[serde(default)]
    registry_url: Option<String>,
    #[serde(default)]
    registry_url_override: Option<String>,
    #[serde(default, deserialize_with = "refuse_runner_url")]
    // Field exists only so `refuse_runner_url` runs on deserialize; it is never read.
    #[allow(dead_code)]
    runner_url: Option<serde::de::IgnoredAny>,
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_at: Option<DateTime<Utc>>,
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    email: Option<String>,
}

fn refuse_runner_url<'de, D>(_: D) -> Result<Option<serde::de::IgnoredAny>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Err(serde::de::Error::custom(
        "runner_url was removed (PL-H8b): delete it from hive_settings.json",
    ))
}

impl From<HiveSettingsWire> for HiveSettingsModel {
    fn from(wire: HiveSettingsWire) -> Self {
        Self {
            hive_url: wire.hive_url.unwrap_or_else(default_hive_url),
            registry_url_override: wire.registry_url_override.or(wire.registry_url),
            token: wire.token,
            refresh_token: wire.refresh_token,
            expires_at: wire.expires_at,
            username: wire.username,
            email: wire.email,
        }
    }
}

fn default_hive_url() -> String {
    DEFAULT_HIVE_URL.to_string()
}

impl Default for HiveSettingsModel {
    fn default() -> Self {
        Self {
            hive_url: default_hive_url(),
            registry_url_override: None,
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
            .field("hive_url", &self.hive_url)
            .field("registry_url_override", &self.registry_url_override)
            .field("token", &redact(&self.token))
            .field("refresh_token", &redact(&self.refresh_token))
            .field("expires_at", &self.expires_at)
            .field("username", &self.username)
            .field("email", &self.email)
            .finish()
    }
}

impl HiveSettingsModel {
    /// The module registry for the marketplace and downloads: the developer
    /// override when set, else the Hive server itself.
    pub fn registry_url(&self) -> &str {
        self.registry_url_override
            .as_deref()
            .unwrap_or(&self.hive_url)
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    /// AGE-697 (PL-H8b): the remote runner is gone, so a settings file still
    /// carrying `runner_url` is refused, and the error names the field.
    #[test]
    fn remote_runner_setting_is_refused() {
        let err = serde_json::from_str::<HiveSettingsModel>(
            r#"{"hive_url":"https://hive.example","runner_url":"https://runner.example"}"#,
        )
        .expect_err("a settings file with runner_url must not load");
        assert!(err.to_string().contains("runner_url"), "{err}");

        // Another unknown key is still ignored.
        let ok: HiveSettingsModel =
            serde_json::from_str(r#"{"hive_url":"https://hive.example","future_key":1}"#)
                .expect("an unknown key other than runner_url still loads");
        assert_eq!(ok.hive_url, "https://hive.example");

        // The model no longer writes the field either.
        let written = serde_json::to_value(HiveSettingsModel::default()).unwrap();
        assert!(written.get("runner_url").is_none(), "{written}");
        serde_json::from_value::<HiveSettingsModel>(written).expect("what is written loads");
    }

    /// AGE-2085: a file written with `registry_url` (the module registry)
    /// loads it as the override and gives `hive_url` its default; a file
    /// with both keys keeps both; saving writes only the new names.
    #[test]
    fn registry_url_becomes_the_override() {
        let old: HiveSettingsModel =
            serde_json::from_str(r#"{"registry_url":"http://localhost:8080","username":"marcel"}"#)
                .unwrap();
        assert_eq!(old.hive_url, DEFAULT_HIVE_URL);
        assert_eq!(
            old.registry_url_override.as_deref(),
            Some("http://localhost:8080")
        );
        assert_eq!(old.registry_url(), "http://localhost:8080");
        assert_eq!(old.username.as_deref(), Some("marcel"));

        let both: HiveSettingsModel = serde_json::from_str(
            r#"{"registry_url":"https://registry.example","hive_url":"https://hive.example"}"#,
        )
        .unwrap();
        assert_eq!(both.hive_url, "https://hive.example");
        assert_eq!(
            both.registry_url_override.as_deref(),
            Some("https://registry.example")
        );

        let none: HiveSettingsModel = serde_json::from_str("{}").unwrap();
        assert_eq!(none.hive_url, DEFAULT_HIVE_URL);
        assert_eq!(none.registry_url_override, None);

        let written = serde_json::to_value(&old).unwrap();
        assert!(written.get("registry_url").is_none(), "{written}");
        assert_eq!(written["hive_url"], DEFAULT_HIVE_URL);
        assert_eq!(written["registry_url_override"], "http://localhost:8080");
        let reloaded: HiveSettingsModel = serde_json::from_value(written).unwrap();
        assert_eq!(reloaded.registry_url_override, old.registry_url_override);
    }

    /// The developer override changes the registry only, never the address
    /// the account signs in at.
    #[test]
    fn registry_override_leaves_the_account_address_alone() {
        let model: HiveSettingsModel = serde_json::from_str(
            r#"{"hive_url":"https://hive.example","registry_url_override":"http://localhost:8080"}"#,
        )
        .unwrap();
        assert_eq!(model.hive_url, "https://hive.example");
        assert_eq!(model.registry_url(), "http://localhost:8080");
    }
}
