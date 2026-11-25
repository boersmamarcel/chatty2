//! The TUI's own Hive sign-in (AGE-2084).
//!
//! The desktop owns the pair in `hive_settings.json` (`hive/*` secret refs)
//! and rotates it. Hive refresh tokens are single use with reuse detection:
//! replaying a spent one revokes every session of the account. So the TUI
//! never touches the desktop's pair; it signs in itself and keeps its own
//! (`hive_tui_settings.json`, `hive-tui/*` refs), refreshed by a real
//! [`HiveSession`] whose every change is written back.

use std::sync::Arc;

use anyhow::{Context, Result};
use hive_client::{HiveRegistryClient, HiveSession, SessionState};

use crate::settings::models::hive_settings::HiveSettingsModel;
use crate::settings::repositories::HiveTuiSettingsRepository;

/// What a signed-out TUI tells the user instead of surfacing a 401.
pub const RUN_LOGIN: &str = "Not signed in to Hive. Run `chatty-tui login` (or /login).";

/// The TUI's session against `hive_url`, starting from the pair it
/// persisted last time. Every later change (a refresh, a login, a logout, a
/// revocation) is saved by a background task, so a rotated refresh token is
/// never lost. Make one per process and share it: two sessions would spend
/// the same refresh token twice.
pub async fn open_session(
    hive_url: &str,
    repo: Arc<dyn HiveTuiSettingsRepository>,
) -> Arc<HiveSession> {
    let mut stored = repo.load().await.unwrap_or_default();
    let session = Arc::new(HiveSession::new(hive_url, stored.token_pair()));
    let mut changes = session.subscribe();
    tokio::spawn(async move {
        while changes.changed().await.is_ok() {
            let pair = match &*changes.borrow_and_update() {
                SessionState::SignedIn(pair) => Some(pair.clone()),
                SessionState::SignedOut | SessionState::Revoked => None,
            };
            stored.set_token_pair(pair.as_ref());
            if let Err(e) = repo.save(stored.clone()).await {
                tracing::warn!(error = ?e, "Failed to save the TUI's Hive sign-in");
            }
        }
    });
    session
}

/// Whether `session` holds a pair.
pub fn is_signed_in(session: &HiveSession) -> bool {
    matches!(&*session.subscribe().borrow(), SessionState::SignedIn(_))
}

/// Sign in with email and password and install the pair on `session`.
/// The address must answer `GET /api/version` like a Hive server first
/// (AGE-2085), the same check the desktop makes.
pub async fn sign_in(
    session: &HiveSession,
    hive_url: &str,
    email: &str,
    password: &str,
) -> Result<()> {
    let client = HiveRegistryClient::new(hive_url);
    client.server_version().await.with_context(|| {
        format!("Could not use {hive_url}; check the Hive address in hive_settings.json")
    })?;
    let pair = client
        .login(email, password)
        .await
        .context("Hive sign-in failed")?;
    session.sign_in(pair).await;
    Ok(())
}

/// The Hive address: the desktop's `hive_settings`, which the TUI only reads.
pub fn hive_url(desktop: &HiveSettingsModel) -> &str {
    &desktop.hive_url
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::repositories::{HiveSettingsJsonRepository, HiveSettingsRepository};
    use hive_client::TokenPair;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn pair(token: &str, refresh: &str, minutes: i64) -> TokenPair {
        TokenPair {
            token: token.to_string(),
            refresh_token: refresh.to_string(),
            expires_at: chrono::Utc::now() + chrono::TimeDelta::minutes(minutes),
        }
    }

    /// A TUI refresh spends the TUI's refresh token and nothing else: the
    /// desktop's never reaches the registry, the TUI's own rotates, and the
    /// rotation is persisted.
    #[tokio::test]
    async fn a_tui_refresh_never_sends_the_desktops_refresh_token() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/auth/refresh"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "token": "tui-access-2",
                "refresh_token": "tui-refresh-2",
                "expires_at": (chrono::Utc::now() + chrono::TimeDelta::hours(1)),
            })))
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().unwrap();
        let desktop = HiveSettingsJsonRepository::with_path(dir.path().join("hive_settings.json"));
        let mut desktop_settings = HiveSettingsModel {
            hive_url: server.uri(),
            ..Default::default()
        };
        desktop_settings.set_token_pair(Some(&pair("desktop-access", "desktop-refresh", 60)));
        desktop.save(desktop_settings.clone()).await.unwrap();

        let tui_repo: Arc<dyn HiveTuiSettingsRepository> = Arc::new(
            crate::settings::repositories::HiveTuiSettingsJsonRepository::with_path(
                dir.path().join("hive_tui_settings.json"),
            ),
        );
        let mut tui_settings = HiveSettingsModel::default();
        // Expiring within the refresh margin, so the next call refreshes.
        tui_settings.set_token_pair(Some(&pair("tui-access-1", "tui-refresh-1", 0)));
        tui_repo.save(tui_settings).await.unwrap();

        let session = open_session(hive_url(&desktop_settings), tui_repo.clone()).await;
        assert_eq!(
            session.access_token().await.as_deref(),
            Some("tui-access-2")
        );

        let bodies: Vec<String> = server
            .received_requests()
            .await
            .unwrap()
            .into_iter()
            .map(|r| String::from_utf8(r.body).unwrap())
            .collect();
        assert_eq!(bodies.len(), 1, "{bodies:?}");
        assert!(bodies[0].contains("tui-refresh-1"), "{bodies:?}");
        assert!(
            bodies.iter().all(|b| !b.contains("desktop-refresh")),
            "{bodies:?}"
        );

        // The rotation is saved (by the background task).
        let mut saved = None;
        for _ in 0..50 {
            let loaded = tui_repo.load().await.unwrap();
            if loaded.refresh_token.as_deref() == Some("tui-refresh-2") {
                saved = Some(loaded);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(saved.is_some(), "the rotated pair was not persisted");

        // The desktop's own file is untouched.
        let desktop_after = desktop.load().await.unwrap();
        assert_eq!(
            desktop_after.refresh_token.as_deref(),
            Some("desktop-refresh")
        );
        assert_eq!(desktop_after.token.as_deref(), Some("desktop-access"));
    }

    /// Signed out until a login installs a pair.
    #[tokio::test]
    async fn signed_out_without_a_stored_pair() {
        let dir = tempfile::tempdir().unwrap();
        let repo: Arc<dyn HiveTuiSettingsRepository> = Arc::new(
            crate::settings::repositories::HiveTuiSettingsJsonRepository::with_path(
                dir.path().join("hive_tui_settings.json"),
            ),
        );
        let session = open_session("http://127.0.0.1:1", repo).await;
        assert!(!is_signed_in(&session));
        assert_eq!(session.access_token().await, None);
    }

    /// AGE-2085: `/login` refuses an address that is not a Hive server before
    /// it sends a password.
    #[tokio::test]
    async fn sign_in_refuses_an_address_that_is_not_a_hive_server() {
        let server = MockServer::start().await; // answers 404 to everything
        let session = HiveSession::new(server.uri(), None);
        let password = uuid::Uuid::new_v4().to_string();
        let err = sign_in(&session, &server.uri(), "me@example.com", &password)
            .await
            .expect_err("not a Hive server");
        assert!(format!("{err:#}").contains("not a Hive server"), "{err:#}");
        let received = server.received_requests().await.unwrap();
        assert!(
            received.iter().all(|r| r.url.path() == "/api/version"),
            "no credentials were sent: {received:?}"
        );
    }
}
