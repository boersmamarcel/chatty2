//! [`HiveConversationsApi`]: the two reads a sync pull makes, over HTTP or
//! scripted for tests.

use std::sync::Arc;

use hive_client::HiveSession;
use hive_client::net::{CappedReadError, Overflow, read_capped};

use super::wire::{ChangesPage, ConversationDetail};
use crate::repositories::conversation_repository::BoxFuture;
use crate::session::hosted::{SIGN_IN_TO_HIVE, refused_body, send_to_server};

/// The most this app reads of one conversation reply from the server: the
/// detail a pull or an open fetches, and the hand-off replies (AGE-2156).
/// Generous on purpose: the server refuses a create or history body over
/// 32 MiB, so a conversation it holds fits in this with room to spare;
/// anything past it is a server or a proxy gone wrong, not a conversation, and
/// is refused instead of held in memory.
pub const MAX_CONVERSATION_BYTES: usize = 64 * 1024 * 1024;

/// `cap` as the user reads it: whole MiB when it is that large.
pub(crate) fn limit_text(cap: usize) -> String {
    const MIB: usize = 1024 * 1024;
    if cap >= MIB && cap.is_multiple_of(MIB) {
        format!("{} MiB", cap / MIB)
    } else {
        format!("{cap} bytes")
    }
}

/// Decode a JSON reply, reading at most `cap` bytes of it through
/// hive-client's capped reader. A longer body is
/// [`CappedReadError::Overflow`] and is never buffered past the cap; a
/// failed read or a body that is not a `T` is
/// [`CappedReadError::Stream`] with the reason.
pub(crate) async fn read_json_capped<T: serde::de::DeserializeOwned>(
    response: reqwest::Response,
    cap: usize,
) -> Result<T, CappedReadError<anyhow::Error>> {
    let body = read_capped(response.bytes_stream(), cap, Overflow::Error, |_| {})
        .await
        .map_err(|error| match error {
            CappedReadError::Overflow => CappedReadError::Overflow,
            CappedReadError::Stream(error) => {
                CappedReadError::Stream(anyhow::anyhow!("reading the reply failed: {error}"))
            }
        })?;
    serde_json::from_slice(&body.bytes)
        .map_err(|error| CappedReadError::Stream(anyhow::anyhow!("unexpected reply: {error}")))
}

/// What [`read_json_capped`]'s overflow says: the reply, and the limit.
fn too_large(what: &str, cap: usize) -> String {
    format!(
        "{what} is larger than {}, the most this app reads from the server",
        limit_text(cap)
    )
}

/// Why a read failed.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// 410 (`cursor_expired`): the cursor is at or before the newest
    /// tombstone the server pruned. The caller pulls again from the start.
    #[error("the sync cursor expired; a full resync is needed")]
    CursorExpired,
    /// 401 after the session's one refresh.
    #[error("{SIGN_IN_TO_HIVE}")]
    SignedOut,
    /// 404 on a detail: the conversation is gone.
    #[error("the conversation does not exist on the server")]
    NotFound,
    /// A resume the server refused (404 or 409); the text is the server's
    /// own `message`, written for the user.
    #[error("{0}")]
    Refused(String),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// The server's conversation list and detail, as a sync pull reads them.
pub trait HiveConversationsApi: Send + Sync {
    /// At most `limit` conversations changed after `cursor`, oldest first;
    /// [`FULL_SYNC_CURSOR`](super::wire::FULL_SYNC_CURSOR) lists from the
    /// start.
    fn changes_since(
        &self,
        cursor: &str,
        limit: usize,
    ) -> BoxFuture<'static, Result<ChangesPage, ApiError>>;

    /// One conversation, in full.
    fn conversation(
        &self,
        remote_id: &str,
    ) -> BoxFuture<'static, Result<ConversationDetail, ApiError>>;

    /// Resume the turn the server kept after a restart (HX-20). The reply is
    /// an SSE stream of the turn's frames; only its status is read, and the
    /// caller follows the turn with `attach()`.
    fn resume(&self, remote_id: &str) -> BoxFuture<'static, Result<(), ApiError>>;
}

/// The API over HTTP, authenticated as the Hive sign-in through
/// [`send_to_server`] (which refuses an insecure URL and retries once after a
/// 401 with a refreshed token).
#[derive(Clone)]
pub struct HttpHiveConversationsApi {
    http: reqwest::Client,
    server_url: String,
    auth: Option<Arc<HiveSession>>,
    /// The most one reply may be ([`MAX_CONVERSATION_BYTES`]).
    max_body: usize,
}

impl HttpHiveConversationsApi {
    pub fn new(server_url: &str, auth: Option<Arc<HiveSession>>) -> Self {
        Self {
            http: reqwest::Client::new(),
            server_url: server_url.trim_end_matches('/').to_string(),
            auth,
            max_body: MAX_CONVERSATION_BYTES,
        }
    }

    /// The same API with a smaller reply limit, so a test need not send
    /// 64 MiB.
    #[cfg(test)]
    fn with_max_body(mut self, max_body: usize) -> Self {
        self.max_body = max_body;
        self
    }

    /// The server this API talks to, without a trailing slash.
    pub fn server_url(&self) -> &str {
        &self.server_url
    }

    fn get<T: serde::de::DeserializeOwned + 'static>(
        &self,
        url: String,
        not_found: bool,
    ) -> BoxFuture<'static, Result<T, ApiError>> {
        let http = self.http.clone();
        let auth = self.auth.clone();
        let max_body = self.max_body;
        Box::pin(async move {
            let response = send_to_server(auth.as_deref(), &url, || http.get(&url)).await?;
            if !response.status().is_success() {
                let (status, _) = refused_body("a sync read", &url, response).await;
                return Err(match status {
                    reqwest::StatusCode::GONE => ApiError::CursorExpired,
                    reqwest::StatusCode::UNAUTHORIZED => ApiError::SignedOut,
                    reqwest::StatusCode::NOT_FOUND if not_found => ApiError::NotFound,
                    status => anyhow::anyhow!("the server answered {status} for {url}").into(),
                });
            }
            read_json_capped::<T>(response, max_body)
                .await
                .map_err(|error| match error {
                    CappedReadError::Overflow => {
                        anyhow::anyhow!(too_large(&format!("the reply from {url}"), max_body))
                            .into()
                    }
                    CappedReadError::Stream(error) => anyhow::anyhow!("{url}: {error:#}").into(),
                })
        })
    }
}

impl HiveConversationsApi for HttpHiveConversationsApi {
    fn changes_since(
        &self,
        cursor: &str,
        limit: usize,
    ) -> BoxFuture<'static, Result<ChangesPage, ApiError>> {
        let base = format!("{}/api/conversations", self.server_url);
        let limit = limit.to_string();
        match reqwest::Url::parse_with_params(&base, [("since", cursor), ("limit", &limit)]) {
            Ok(url) => self.get(url.to_string(), false),
            Err(error) => {
                let error = anyhow::anyhow!("bad server URL {base}: {error}");
                Box::pin(async move { Err(error.into()) })
            }
        }
    }

    fn conversation(
        &self,
        remote_id: &str,
    ) -> BoxFuture<'static, Result<ConversationDetail, ApiError>> {
        self.get(
            format!("{}/api/conversations/{remote_id}", self.server_url),
            true,
        )
    }

    fn resume(&self, remote_id: &str) -> BoxFuture<'static, Result<(), ApiError>> {
        let http = self.http.clone();
        let auth = self.auth.clone();
        let url = format!(
            "{}/api/conversations/{remote_id}/turn/resume",
            self.server_url
        );
        Box::pin(async move {
            let response = send_to_server(auth.as_deref(), &url, || {
                http.post(&url).json(&serde_json::json!({}))
            })
            .await?;
            if response.status().is_success() {
                return Ok(());
            }
            let (status, body) = refused_body("a turn resume", &url, response).await;
            if status == reqwest::StatusCode::UNAUTHORIZED {
                return Err(ApiError::SignedOut);
            }
            if status == reqwest::StatusCode::SERVICE_UNAVAILABLE {
                return Err(ApiError::Refused(
                    "The server is restarting. Try again in a few seconds.".to_string(),
                ));
            }
            if status == reqwest::StatusCode::NOT_FOUND || status == reqwest::StatusCode::CONFLICT {
                let message = serde_json::from_str::<serde_json::Value>(&body)
                    .ok()
                    .and_then(|body| body.get("message")?.as_str().map(str::to_string))
                    .unwrap_or_else(|| "This turn can't be resumed any more.".to_string());
                return Err(ApiError::Refused(message));
            }
            Err(anyhow::anyhow!("the server answered {status} for {url}").into())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::hive_sync::wire::RemoteChange;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn signed_in(server: &MockServer) -> Arc<HiveSession> {
        Arc::new(HiveSession::new(
            server.uri(),
            Some(hive_client::TokenPair {
                token: "tok".into(),
                refresh_token: "r".into(),
                expires_at: chrono::Utc::now() + chrono::TimeDelta::hours(1),
            }),
        ))
    }

    #[tokio::test]
    async fn pulls_a_page_with_the_cursor_and_the_bearer() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/conversations"))
            .and(query_param("since", "c 1"))
            .and(query_param("limit", "50"))
            .and(header("authorization", "Bearer tok"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "conversations": [{"id": "r-1", "deleted": true}],
                "next_cursor": "c-2"
            })))
            .expect(1)
            .mount(&server)
            .await;
        let api =
            HttpHiveConversationsApi::new(&format!("{}/", server.uri()), Some(signed_in(&server)));

        let page = api.changes_since("c 1", 50).await.expect("a page");
        assert_eq!(
            page.changes,
            vec![RemoteChange::Deleted { id: "r-1".into() }]
        );
        assert_eq!(page.next_cursor.as_deref(), Some("c-2"));
    }

    #[tokio::test]
    async fn status_codes_map_to_typed_errors() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/conversations"))
            .respond_with(ResponseTemplate::new(410))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/conversations/gone"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/conversations/locked"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        let api = HttpHiveConversationsApi::new(&server.uri(), None);

        assert!(matches!(
            api.changes_since("old", 50).await,
            Err(ApiError::CursorExpired)
        ));
        assert!(matches!(
            api.conversation("gone").await,
            Err(ApiError::NotFound)
        ));
        let signed_out = api.conversation("locked").await.unwrap_err();
        assert_eq!(signed_out.to_string(), SIGN_IN_TO_HIVE);
    }

    /// AGE-2156: the detail is read through the capped reader. One over the
    /// limit is a clear error naming it; one at the limit is read.
    #[tokio::test]
    async fn a_detail_over_the_limit_is_refused_with_a_clear_error() {
        let server = MockServer::start().await;
        let detail = serde_json::json!({"id": "r-1", "title": "x".repeat(4096)}).to_string();
        Mock::given(method("GET"))
            .and(path("/api/conversations/r-1"))
            .respond_with(ResponseTemplate::new(200).set_body_string(detail.clone()))
            .mount(&server)
            .await;

        let small = HttpHiveConversationsApi::new(&server.uri(), None).with_max_body(1024);
        let error = small.conversation("r-1").await.unwrap_err();
        assert!(matches!(error, ApiError::Other(_)), "{error:?}");
        assert!(
            error
                .to_string()
                .contains("is larger than 1024 bytes, the most this app reads from the server"),
            "{error}"
        );

        let exact = HttpHiveConversationsApi::new(&server.uri(), None).with_max_body(detail.len());
        assert_eq!(exact.conversation("r-1").await.unwrap().id, "r-1");
        assert_eq!(limit_text(MAX_CONVERSATION_BYTES), "64 MiB");
    }
    #[tokio::test]
    async fn resume_posts_to_the_route_and_a_refusal_carries_the_servers_message() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/conversations/ok/turn/resume"))
            .and(header("authorization", "Bearer tok"))
            .respond_with(ResponseTemplate::new(202))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/conversations/gone/turn/resume"))
            .respond_with(ResponseTemplate::new(409).set_body_json(
                serde_json::json!({"error": "not_interrupted", "message": "That turn already finished."}),
            ))
            .mount(&server)
            .await;
        let api = HttpHiveConversationsApi::new(&server.uri(), Some(signed_in(&server)));

        Mock::given(method("POST"))
            .and(path("/api/conversations/draining/turn/resume"))
            .respond_with(
                ResponseTemplate::new(503).set_body_json(serde_json::json!({"error": "draining"})),
            )
            .mount(&server)
            .await;
        api.resume("ok").await.unwrap();
        match api.resume("draining").await {
            Err(ApiError::Refused(message)) => {
                assert_eq!(
                    message,
                    "The server is restarting. Try again in a few seconds."
                )
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
        match api.resume("gone").await {
            Err(ApiError::Refused(message)) => assert_eq!(message, "That turn already finished."),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }
}
