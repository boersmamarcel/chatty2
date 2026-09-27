//! The signed-in Hive session: one access/refresh token pair shared by every
//! authenticated caller.
//!
//! hive-registry issues a one-hour access token and a 30-day refresh token
//! that rotates on every use; presenting a retired refresh token is reuse and
//! revokes every refresh token of the user. So the pair lives in exactly one
//! [`HiveSession`], and the refresh runs under the lock that guards it:
//! concurrent callers that all see an expiring token or a 401 share one
//! refresh instead of racing each other into reuse detection.

use chrono::Utc;
use tokio::sync::{Mutex, watch};

use crate::{client::HiveRegistryClient, error::ClientError, models::TokenPair};

/// Refresh proactively once the access token has less than this left.
pub const REFRESH_MARGIN: chrono::TimeDelta = chrono::TimeDelta::seconds(60);

/// What the session holds, broadcast on every change (see
/// [`HiveSession::subscribe`]) so the owner can persist a rotated pair and
/// tell the user when they have been signed out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionState {
    /// No pair: never signed in, or signed out on purpose.
    SignedOut,
    /// Signed in with this pair (after a login or a refresh).
    SignedIn(TokenPair),
    /// The registry rejected the refresh token (expired, revoked, or reuse
    /// detected), so the session was cleared. The user must sign in again.
    Revoked,
}

/// Holder of the Hive token pair; share it as an `Arc` between every client
/// that talks to the registry on the user's behalf.
pub struct HiveSession {
    registry: HiveRegistryClient,
    pair: Mutex<Option<TokenPair>>,
    state: watch::Sender<SessionState>,
}

impl HiveSession {
    /// A session against the registry at `base_url`, starting from a pair
    /// persisted by an earlier run (or none).
    pub fn new(base_url: impl Into<String>, pair: Option<TokenPair>) -> Self {
        let state = match &pair {
            Some(pair) => SessionState::SignedIn(pair.clone()),
            None => SessionState::SignedOut,
        };
        Self {
            registry: HiveRegistryClient::new(base_url),
            pair: Mutex::new(pair),
            state: watch::Sender::new(state),
        }
    }

    /// Receive every change of the session's state.
    pub fn subscribe(&self) -> watch::Receiver<SessionState> {
        self.state.subscribe()
    }

    /// Install the pair a login or registration returned.
    pub async fn sign_in(&self, pair: TokenPair) {
        let mut guard = self.pair.lock().await;
        *guard = Some(pair.clone());
        self.state.send_replace(SessionState::SignedIn(pair));
    }

    /// Drop the pair and revoke its refresh token on the registry. The
    /// revocation is best effort: the local session is cleared either way.
    pub async fn sign_out(&self) {
        let mut guard = self.pair.lock().await;
        let pair = guard.take();
        self.state.send_replace(SessionState::SignedOut);
        if let Some(pair) = pair
            && let Err(e) = self.registry.logout(&pair.refresh_token).await
        {
            tracing::warn!(error = %e, "Failed to revoke the Hive refresh token on sign-out");
        }
    }

    /// The access token to send, refreshed first when it expires within
    /// [`REFRESH_MARGIN`]. `None` when signed out.
    pub async fn access_token(&self) -> Option<String> {
        let mut guard = self.pair.lock().await;
        let expiring = guard
            .as_ref()
            .is_some_and(|pair| pair.expires_at - Utc::now() < REFRESH_MARGIN);
        if expiring {
            self.refresh_locked(&mut guard).await;
        }
        guard.as_ref().map(|pair| pair.token.clone())
    }

    /// The token to retry with after the registry answered `rejected` with a
    /// 401: the pair another caller already refreshed to, or a freshly
    /// refreshed one. `None` means there is nothing new to retry with.
    pub(crate) async fn token_after_401(&self, rejected: &str) -> Option<String> {
        let mut guard = self.pair.lock().await;
        match guard.as_ref() {
            None => return None,
            Some(pair) if pair.token != rejected => return Some(pair.token.clone()),
            Some(_) => {}
        }
        self.refresh_locked(&mut guard).await;
        guard
            .as_ref()
            .map(|pair| pair.token.clone())
            .filter(|token| token != rejected)
    }

    /// Exchange the held refresh token for a new pair. A rejected refresh
    /// token clears the session; an unreachable registry keeps the old pair
    /// so the next call can try again.
    async fn refresh_locked(&self, guard: &mut Option<TokenPair>) {
        let Some(current) = guard.as_ref() else {
            return;
        };
        match self.registry.refresh(&current.refresh_token).await {
            Ok(pair) => {
                *guard = Some(pair.clone());
                self.state.send_replace(SessionState::SignedIn(pair));
            }
            Err(ClientError::Unauthorized) => {
                *guard = None;
                self.state.send_replace(SessionState::Revoked);
                tracing::warn!("Signed out of Hive: the registry rejected the refresh token");
            }
            Err(e) => {
                tracing::warn!(error = %e, "Hive token refresh failed; keeping the current token");
            }
        }
    }
}

/// Send the request `build` makes with the session's Bearer token; on a 401,
/// refresh once and send it once more. Without a session the request goes
/// out unauthenticated, as it did before sign-in existed.
pub(crate) async fn send_authed(
    session: Option<&HiveSession>,
    build: impl Fn() -> reqwest::RequestBuilder,
) -> Result<reqwest::Response, reqwest::Error> {
    let token = match session {
        Some(session) => session.access_token().await,
        None => None,
    };
    let response = with_bearer(build(), token.as_deref()).send().await?;
    if response.status() != reqwest::StatusCode::UNAUTHORIZED {
        return Ok(response);
    }
    let (Some(session), Some(rejected)) = (session, token) else {
        return Ok(response);
    };
    match session.token_after_401(&rejected).await {
        Some(fresh) => with_bearer(build(), Some(&fresh)).send().await,
        None => Ok(response),
    }
}

fn with_bearer(request: reqwest::RequestBuilder, token: Option<&str>) -> reqwest::RequestBuilder {
    match token {
        Some(token) => request.bearer_auth(token),
        None => request,
    }
}
