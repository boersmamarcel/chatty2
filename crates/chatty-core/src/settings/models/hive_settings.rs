use chrono::{DateTime, Utc};
use hive_client::TokenPair;
use serde::{Deserialize, Serialize};

pub const DEFAULT_REGISTRY_URL: &str = "http://localhost:8080";

/// Settings for the Hive module registry connection and account.
///
/// `Debug` is redacted: the tokens may never reach a log.
///
/// A file still carrying the removed `runner_url` (PL-H8b) fails to load with
/// an error naming it; any other unknown key is ignored as before.
#[derive(Clone, Serialize, Deserialize)]
#[serde(try_from = "HiveSettingsWire")]
pub struct HiveSettingsModel {
    /// Base URL of the Hive registry.
    #[serde(default = "default_registry_url")]
    pub registry_url: String,
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
    /// Whether a conversation started while signed in is synced to the
    /// account (AGE-2088; Marcel, 2026-11-25: on by default, with a
    /// per-conversation "Keep on this device"). Pushing its turns is HX-17.
    /// Left out of the file at its default, so existing files keep their
    /// bytes.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub sync_new_conversations: bool,
    /// Whether the one-time offer to sync the conversations that existed
    /// before sign-in has been shown; they stay local-only until accepted.
    #[serde(default, skip_serializing_if = "is_false")]
    pub sync_existing_offered: bool,
}

/// What a settings file holds: the model's fields plus the removed
/// `runner_url`, which only exists to be refused.
#[derive(Deserialize)]
struct HiveSettingsWire {
    #[serde(default = "default_registry_url")]
    registry_url: String,
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
    #[serde(default = "default_true")]
    sync_new_conversations: bool,
    #[serde(default)]
    sync_existing_offered: bool,
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
            registry_url: wire.registry_url,
            token: wire.token,
            refresh_token: wire.refresh_token,
            expires_at: wire.expires_at,
            username: wire.username,
            email: wire.email,
            sync_new_conversations: wire.sync_new_conversations,
            sync_existing_offered: wire.sync_existing_offered,
        }
    }
}

fn default_true() -> bool {
    true
}

fn is_true(value: &bool) -> bool {
    *value
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn default_registry_url() -> String {
    DEFAULT_REGISTRY_URL.to_string()
}

impl Default for HiveSettingsModel {
    fn default() -> Self {
        Self {
            registry_url: default_registry_url(),
            token: None,
            refresh_token: None,
            expires_at: None,
            username: None,
            email: None,
            sync_new_conversations: true,
            sync_existing_offered: false,
        }
    }
}

impl std::fmt::Debug for HiveSettingsModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let redact = |t: &Option<String>| t.as_ref().map(|_| "<redacted>");
        f.debug_struct("HiveSettingsModel")
            .field("registry_url", &self.registry_url)
            .field("token", &redact(&self.token))
            .field("refresh_token", &redact(&self.refresh_token))
            .field("expires_at", &self.expires_at)
            .field("username", &self.username)
            .field("email", &self.email)
            .field("sync_new_conversations", &self.sync_new_conversations)
            .field("sync_existing_offered", &self.sync_existing_offered)
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

    /// Is a conversation started now synced to the account? Only while
    /// signed in, and only when the user has not turned it off.
    pub fn syncs_new_conversations(&self) -> bool {
        self.is_logged_in() && self.sync_new_conversations
    }

    /// Should the app offer, once, to sync the conversations that existed
    /// before sign-in?
    pub fn should_offer_to_sync_existing(&self) -> bool {
        self.is_logged_in() && !self.sync_existing_offered
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
            r#"{"registry_url":"https://hive.example","runner_url":"https://runner.example"}"#,
        )
        .expect_err("a settings file with runner_url must not load");
        assert!(err.to_string().contains("runner_url"), "{err}");

        // Another unknown key is still ignored.
        let ok: HiveSettingsModel =
            serde_json::from_str(r#"{"registry_url":"https://hive.example","future_key":1}"#)
                .expect("an unknown key other than runner_url still loads");
        assert_eq!(ok.registry_url, "https://hive.example");

        // The model no longer writes the field either.
        let written = serde_json::to_value(HiveSettingsModel::default()).unwrap();
        assert!(written.get("runner_url").is_none(), "{written}");
        serde_json::from_value::<HiveSettingsModel>(written).expect("what is written loads");
    }

    /// AGE-2088: new conversations sync by default once signed in, existing
    /// ones are offered once, and a file written before the setting keeps
    /// its bytes on the next save.
    #[test]
    fn sync_defaults_on_for_new_conversations_and_stays_out_of_old_files() {
        let old: HiveSettingsModel =
            serde_json::from_str(r#"{"registry_url":"http://localhost:8080"}"#).unwrap();
        assert!(old.sync_new_conversations);
        assert!(!old.sync_existing_offered);
        assert!(!old.syncs_new_conversations(), "signed out, nothing syncs");
        assert!(!old.should_offer_to_sync_existing());
        let json = serde_json::to_value(&old).unwrap();
        assert!(json.get("sync_new_conversations").is_none());
        assert!(json.get("sync_existing_offered").is_none());

        let signed_in = HiveSettingsModel {
            token: Some("t".into()),
            ..old
        };
        assert!(signed_in.syncs_new_conversations());
        assert!(signed_in.should_offer_to_sync_existing());

        let opted_out = HiveSettingsModel {
            sync_new_conversations: false,
            sync_existing_offered: true,
            ..signed_in
        };
        assert!(!opted_out.syncs_new_conversations());
        assert!(!opted_out.should_offer_to_sync_existing());
        let round_trip: HiveSettingsModel =
            serde_json::from_value(serde_json::to_value(&opted_out).unwrap()).unwrap();
        assert!(!round_trip.sync_new_conversations);
        assert!(round_trip.sync_existing_offered);
    }
}
