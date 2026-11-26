//! Pushing a device-held conversation's history to the account (AGE-2096,
//! HX-17).
//!
//! After each finished turn of a synced conversation whose placement is this
//! device, the history goes to the server with `PUT …/history` and
//! `If-Match: <version>`. The push is best-effort and never part of the turn:
//!
//! * [`PushQueue`] — pure: one pending push per conversation, in the order
//!   conversations first asked, with the conversations the server said another
//!   device holds set aside;
//! * [`HiveHistoryApi`] — the one write, over HTTP ([`HttpHiveHistoryApi`]) or
//!   scripted;
//! * [`settle`](PushQueue::settle) turns a reply into a [`PushEvent`] for the
//!   owner: 412 and 409 are never overwritten, 412 asks for a pull, 409 stops
//!   pushing that conversation, and a lost connection leaves the push queued.
//!
//! A conversation that is not `Synced` with a `Device` placement for this
//! device has no job ([`PushJob::for_device`] is `None`), so a LocalOnly
//! conversation makes no request at all.

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;

use hive_client::HiveSession;

use super::wire::{ConflictBody, CreatedConversation, HistoryAccepted};
use crate::models::conversation::{ConversationSync, Placement};
use crate::repositories::ConversationData;
use crate::repositories::conversation_repository::BoxFuture;
use crate::session::hosted::{LOG_TARGET, SIGN_IN_TO_HIVE, error_code, send_to_server};

/// What a deleted conversation's 410 says when the server sends no message.
pub const DELETED_ON_ACCOUNT: &str =
    "This conversation was deleted from your Hive account. It stays on this computer.";

/// Why a push did not land.
#[derive(Debug, thiserror::Error)]
pub enum PushError {
    /// 412: the server's history moved on since `If-Match`. Pull, never
    /// overwrite.
    #[error("the server has a newer version of the conversation")]
    Stale,
    /// 409: another device holds the conversation now.
    #[error("another device holds the conversation")]
    HeldElsewhere(Box<ConflictBody>),
    /// 410 on a create: the conversation was deleted on the account and its
    /// id stays retired. It becomes local-only, the local copy kept; the
    /// string is the server's message, shown once.
    #[error("{0}")]
    Deleted(String),
    /// 401 after the session's one refresh.
    #[error("{SIGN_IN_TO_HIVE}")]
    SignedOut,
    /// The server could not be reached: the push waits.
    #[error("could not reach the server: {0}")]
    Offline(String),
    /// The server refused the push for a reason a retry will not change.
    #[error("the server refused the push: {0}")]
    Rejected(String),
}

/// The history write.
pub trait HiveHistoryApi: Send + Sync {
    /// Replace the history of `remote_id`, if the server is still at
    /// `if_match`.
    fn put_history(
        &self,
        remote_id: &str,
        if_match: u64,
        body: &ConversationData,
    ) -> BoxFuture<'static, Result<HistoryAccepted, PushError>>;

    /// Put a local conversation on the account, held by this device: a
    /// create with `client_id` (the local id, so a retry after a lost reply
    /// finds the same conversation) and the full conversation.
    fn create(
        &self,
        client_id: &str,
        body: &ConversationData,
    ) -> BoxFuture<'static, Result<CreatedConversation, PushError>>;
}

/// The reply of a push request as a [`PushError`], or the success to decode.
fn classify(status: reqwest::StatusCode) -> Option<PushError> {
    match status {
        status if status.is_success() => None,
        reqwest::StatusCode::UNAUTHORIZED => Some(PushError::SignedOut),
        status if status.is_server_error() => {
            Some(PushError::Offline(format!("the server answered {status}")))
        }
        status => Some(PushError::Rejected(format!("the server answered {status}"))),
    }
}

/// One line per refused push or create: the conversation, the status and the
/// server's error code (never its message, which can quote the user). A 409
/// or 412 is the protocol working, so it is `info`; anything else `warn`.
fn log_refusal(what: &str, id: &str, if_match: u64, status: reqwest::StatusCode, body: &str) {
    let code = error_code(body);
    let status = status.as_u16();
    if matches!(status, 409 | 412) {
        tracing::info!(target: LOG_TARGET, remote_id = id, if_match, status, error_code = code.as_deref(), "The server refused {what}");
    } else {
        tracing::warn!(target: LOG_TARGET, remote_id = id, if_match, status, error_code = code.as_deref(), "The server refused {what}");
    }
}

/// [`HiveHistoryApi`] over HTTP, authenticated as the Hive sign-in through
/// [`send_to_server`] (which refuses an insecure URL).
#[derive(Clone)]
pub struct HttpHiveHistoryApi {
    http: reqwest::Client,
    server_url: String,
    auth: Option<Arc<HiveSession>>,
}

impl HttpHiveHistoryApi {
    pub fn new(server_url: &str, auth: Option<Arc<HiveSession>>) -> Self {
        Self {
            http: reqwest::Client::new(),
            server_url: server_url.trim_end_matches('/').to_string(),
            auth,
        }
    }
}

impl HiveHistoryApi for HttpHiveHistoryApi {
    fn put_history(
        &self,
        remote_id: &str,
        if_match: u64,
        body: &ConversationData,
    ) -> BoxFuture<'static, Result<HistoryAccepted, PushError>> {
        let url = format!("{}/api/conversations/{remote_id}/history", self.server_url);
        let remote_id = remote_id.to_string();
        let http = self.http.clone();
        let auth = self.auth.clone();
        let body = serde_json::to_value(body);
        Box::pin(async move {
            let body = body.map_err(|error| PushError::Rejected(error.to_string()))?;
            // An insecure URL is a configuration error, not a dropped
            // connection: it must not sit in the queue waiting to come back.
            hive_client::ensure_secure_url(&url).map_err(PushError::Rejected)?;
            let response = send_to_server(auth.as_deref(), &url, || {
                http.put(&url)
                    .header(reqwest::header::IF_MATCH, if_match.to_string())
                    .json(&body)
            })
            .await
            .map_err(|error| {
                tracing::warn!(target: LOG_TARGET, remote_id, if_match, error = format!("{error:#}"), "Could not send a history push");
                PushError::Offline(format!("{error:#}"))
            })?;
            let status = response.status();
            if status.is_success() {
                return response
                    .json::<HistoryAccepted>()
                    .await
                    .map_err(|error| PushError::Rejected(format!("unexpected reply: {error}")));
            }
            let text = response.text().await.unwrap_or_default();
            log_refusal("a history push", &remote_id, if_match, status, &text);
            match status {
                reqwest::StatusCode::PRECONDITION_FAILED => Err(PushError::Stale),
                reqwest::StatusCode::CONFLICT => Err(PushError::HeldElsewhere(Box::new(
                    serde_json::from_str::<ConflictBody>(&text).unwrap_or_default(),
                ))),
                reqwest::StatusCode::UNAUTHORIZED => Err(PushError::SignedOut),
                status if status.is_server_error() => {
                    Err(PushError::Offline(format!("the server answered {status}")))
                }
                status => Err(PushError::Rejected(format!("the server answered {status}"))),
            }
        })
    }

    fn create(
        &self,
        client_id: &str,
        body: &ConversationData,
    ) -> BoxFuture<'static, Result<CreatedConversation, PushError>> {
        let url = format!("{}/api/conversations", self.server_url);
        let request = serde_json::to_value(body).map(|conversation| {
            serde_json::json!({
                "client_id": client_id,
                "conversation": conversation,
                "placement": "device",
            })
        });
        let (http, auth) = (self.http.clone(), self.auth.clone());
        Box::pin(async move {
            let request = request.map_err(|error| PushError::Rejected(error.to_string()))?;
            Self::create_at(http, auth, url, request).await
        })
    }
}

impl HttpHiveHistoryApi {
    async fn create_at(
        http: reqwest::Client,
        auth: Option<Arc<HiveSession>>,
        url: String,
        body: serde_json::Value,
    ) -> Result<CreatedConversation, PushError> {
        hive_client::ensure_secure_url(&url).map_err(PushError::Rejected)?;
        let conv_id = body
            .get("client_id")
            .and_then(|id| id.as_str())
            .unwrap_or_default()
            .to_string();
        let response = send_to_server(auth.as_deref(), &url, || http.post(&url).json(&body))
            .await
            .map_err(|error| {
                tracing::warn!(target: LOG_TARGET, conv_id, error = format!("{error:#}"), "Could not send a conversation create");
                PushError::Offline(format!("{error:#}"))
            })?;
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            tracing::warn!(
                target: LOG_TARGET,
                conv_id,
                status = status.as_u16(),
                error_code = error_code(&text).as_deref(),
                "The server refused a conversation create"
            );
            return Err(create_refusal(status, &text));
        }
        response
            .json::<CreatedConversation>()
            .await
            .map_err(|error| PushError::Rejected(format!("unexpected reply: {error}")))
    }
}

/// A refused create as a [`PushError`].
fn create_refusal(status: reqwest::StatusCode, body: &str) -> PushError {
    if status == reqwest::StatusCode::GONE {
        let body: ConflictBody = serde_json::from_str(body).unwrap_or_default();
        return PushError::Deleted(
            body.message
                .filter(|message| !message.trim().is_empty())
                .unwrap_or_else(|| DELETED_ON_ACCOUNT.to_string()),
        );
    }
    classify(status).unwrap_or_else(|| PushError::Rejected(format!("the server answered {status}")))
}

/// Where one push goes: the conversation's address on the server and the
/// version this device's history is at (the `If-Match`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PushTarget {
    pub server_url: String,
    pub remote_id: String,
    pub version: u64,
}

impl PushTarget {
    /// The target for a conversation, or `None` when this device is not the
    /// one holding it: a [`LocalOnly`](ConversationSync::LocalOnly)
    /// conversation, one that runs in the cloud, or one placed on another
    /// device never makes a request.
    pub fn for_device(sync: &ConversationSync, this_device: &str) -> Option<Self> {
        let synced = sync.synced()?;
        match &synced.placement {
            Placement::Device { device_id } if device_id == this_device => Some(Self {
                server_url: synced.server_url.clone(),
                remote_id: synced.remote_id.clone(),
                version: synced.version,
            }),
            _ => None,
        }
    }
}

/// What one push needs, read from the conversation when its turn comes.
#[derive(Clone, Debug)]
pub struct PushJob {
    pub remote_id: String,
    /// The server version this device's history is at: the `If-Match`.
    pub version: u64,
    pub body: ConversationData,
}

impl PushJob {
    /// The job for a conversation, or `None` when this device does not hold
    /// it ([`PushTarget::for_device`]).
    pub fn for_device(
        sync: &ConversationSync,
        this_device: &str,
        body: impl FnOnce() -> ConversationData,
    ) -> Option<Self> {
        let target = PushTarget::for_device(sync, this_device)?;
        Some(Self {
            remote_id: target.remote_id,
            version: target.version,
            body: body(),
        })
    }
}

/// What a settled push means for the owner.
#[derive(Clone, Debug, PartialEq)]
pub enum PushEvent {
    /// The server took the history at `version`: record it as the
    /// conversation's version.
    Pushed { local_id: String, version: u64 },
    /// 412: pull the conversation, then [`enqueue`](PushQueue::enqueue) it
    /// again if this device still holds it.
    NeedsPull { local_id: String },
    /// 409: another device holds it; this conversation is no longer pushed
    /// until [`release`](PushQueue::release).
    HeldElsewhere {
        local_id: String,
        held_by: ConflictBody,
    },
    /// 401: nothing more is pushed until the user signs in. The push stays
    /// queued.
    SignedOut,
    /// The server could not be reached. The push stays queued, in place.
    Offline,
    /// The server refused it for good; it is dropped, and the next turn
    /// queues a fresh one.
    Dropped { local_id: String, reason: String },
    /// 410: deleted on the account. Make it local-only, keep the local copy,
    /// and show `message` once.
    Deleted { local_id: String, message: String },
}

/// What a frontend with no queue does after a finished turn (chatty-tui):
/// push the history of a conversation this device holds, or create a new
/// conversation on its first turn. Returns the sync state to record, or
/// `None` when there was nothing to send, which includes every local-only
/// conversation past its first turn.
///
/// The caller has already checked that the user is signed in and that new
/// conversations sync.
pub async fn sync_after_turn<A: HiveHistoryApi + ?Sized>(
    api: &A,
    server_url: &str,
    this_device: &str,
    local_id: &str,
    sync: &ConversationSync,
    first_turn: bool,
    body: ConversationData,
) -> Result<Option<ConversationSync>, PushError> {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;
    let body = super::wire::push_body(body);
    match sync {
        ConversationSync::Synced(synced) => {
            let Some(target) = PushTarget::for_device(sync, this_device) else {
                return Ok(None);
            };
            let accepted = api
                .put_history(&target.remote_id, target.version, &body)
                .await?;
            let mut synced = synced.clone();
            synced.version = accepted.version;
            synced.last_synced_at = Some(now_ms);
            Ok(Some(ConversationSync::Synced(synced)))
        }
        ConversationSync::LocalOnly if first_turn => {
            let created = api.create(local_id, &body).await?;
            Ok(Some(ConversationSync::Synced(
                crate::models::conversation::SyncedConversation {
                    server_url: server_url.trim_end_matches('/').to_string(),
                    remote_id: created.id,
                    version: created.version,
                    placement: Placement::Device {
                        device_id: this_device.to_string(),
                    },
                    last_synced_at: Some(now_ms),
                },
            )))
        }
        ConversationSync::LocalOnly => Ok(None),
    }
}

/// [`sync_after_turn`] answered 412 (AGE-2122): the account's copy moved on
/// since this device last read it. Only the holder writes the history, so
/// what changed elsewhere is metadata, such as a rename on the web. Read the
/// account's copy and, while this device still holds it, push again on its
/// version with its title. `Ok(None)` when another device holds it now.
pub async fn sync_after_stale<H, C>(
    history: &H,
    conversations: &C,
    server_url: &str,
    this_device: &str,
    local_id: &str,
    sync: &ConversationSync,
    mut body: ConversationData,
) -> Result<Option<ConversationSync>, PushError>
where
    H: HiveHistoryApi + ?Sized,
    C: super::api::HiveConversationsApi + ?Sized,
{
    let Some(synced) = sync.synced() else {
        return Ok(None);
    };
    let detail = conversations
        .conversation(&synced.remote_id)
        .await
        .map_err(|error| PushError::Offline(error.to_string()))?;
    let held_here = matches!(
        &detail.placement,
        Some(Placement::Device { device_id }) if device_id == this_device
    );
    let Some(version) = detail.version.filter(|_| held_here) else {
        return Ok(None);
    };
    let mut synced = synced.clone();
    synced.version = version;
    body.title = detail.title;
    sync_after_turn(
        history,
        server_url,
        this_device,
        local_id,
        &ConversationSync::Synced(synced),
        false,
        body,
    )
    .await
}

/// Pending pushes: one per conversation, oldest request first.
#[derive(Clone, Debug, Default)]
pub struct PushQueue {
    pending: VecDeque<String>,
    /// Conversations another device holds: not pushed, not queued.
    held: HashSet<String>,
}

impl PushQueue {
    /// Ask for `local_id`'s history to be pushed. A conversation already
    /// waiting keeps its place (its job reads the latest history when it
    /// runs, so one push covers every turn since); one the server said
    /// another device holds is ignored.
    pub fn enqueue(&mut self, local_id: &str) {
        if self.held.contains(local_id) || self.pending.iter().any(|id| id == local_id) {
            return;
        }
        self.pending.push_back(local_id.to_string());
    }

    /// The user took the conversation back, or it was pulled and is ours
    /// again: it may be pushed.
    pub fn release(&mut self, local_id: &str) {
        self.held.remove(local_id);
    }

    /// Drop everything for a conversation that was deleted or turned
    /// local-only.
    pub fn forget(&mut self, local_id: &str) {
        self.pending.retain(|id| id != local_id);
        self.held.remove(local_id);
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    pub fn len(&self) -> usize {
        self.pending.len()
    }

    /// Is `local_id` set aside because another device holds it?
    pub fn is_held(&self, local_id: &str) -> bool {
        self.held.contains(local_id)
    }

    /// The conversation to push next. It stays out of the queue while its
    /// push is in flight; [`settle`](Self::settle) puts it back if the push
    /// must wait.
    pub fn take(&mut self) -> Option<String> {
        self.pending.pop_front()
    }

    /// Record how the push of `local_id` (taken with [`take`](Self::take))
    /// went.
    pub fn settle(
        &mut self,
        local_id: &str,
        result: Result<HistoryAccepted, PushError>,
    ) -> PushEvent {
        let local_id = local_id.to_string();
        match result {
            Ok(accepted) => PushEvent::Pushed {
                local_id,
                version: accepted.version,
            },
            Err(PushError::Stale) => PushEvent::NeedsPull { local_id },
            Err(PushError::HeldElsewhere(held_by)) => {
                self.held.insert(local_id.clone());
                PushEvent::HeldElsewhere {
                    local_id,
                    held_by: *held_by,
                }
            }
            Err(PushError::SignedOut) => {
                self.pending.push_front(local_id);
                PushEvent::SignedOut
            }
            Err(PushError::Offline(_)) => {
                self.pending.push_front(local_id);
                PushEvent::Offline
            }
            Err(PushError::Rejected(reason)) => PushEvent::Dropped { local_id, reason },
            Err(PushError::Deleted(message)) => PushEvent::Deleted { local_id, message },
        }
    }
}

/// Push every queued conversation, in order, until the queue is empty or a
/// push cannot go (offline, signed out).
///
/// `job` reads a conversation's push when its turn comes: a conversation with
/// none (local-only, no longer held) is skipped without a request, and the
/// history is the latest, not the one at enqueue time. `on_event` hears each
/// outcome before the next push starts, so a 412's pull can run first. The
/// frontend that cannot hold its store across an await (the desktop) drives
/// [`PushQueue::take`] / [`PushQueue::settle`] itself.
pub async fn drain<A, J, E>(api: &A, queue: &mut PushQueue, mut job: J, mut on_event: E)
where
    A: HiveHistoryApi + ?Sized,
    J: FnMut(&str) -> Option<PushJob>,
    E: FnMut(&mut PushQueue, PushEvent),
{
    while let Some(local_id) = queue.take() {
        let Some(push) = job(&local_id) else {
            continue;
        };
        let result = api
            .put_history(&push.remote_id, push.version, &push.body)
            .await;
        let event = queue.settle(&local_id, result);
        let stop = matches!(event, PushEvent::SignedOut | PushEvent::Offline);
        on_event(queue, event);
        if stop {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use wiremock::matchers::{body_partial_json, header, method, path};
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

    fn device_sync(version: u64, device: &str) -> ConversationSync {
        ConversationSync::Synced(crate::models::conversation::SyncedConversation {
            server_url: "http://localhost".into(),
            remote_id: "r-1".into(),
            version,
            placement: Placement::Device {
                device_id: device.into(),
            },
            last_synced_at: None,
        })
    }

    fn body(title: &str) -> ConversationData {
        crate::services::hive_sync::ConversationDetail {
            title: title.into(),
            ..Default::default()
        }
        .to_conversation_data("local")
    }

    fn job(version: u64) -> PushJob {
        PushJob {
            remote_id: "r-1".into(),
            version,
            body: body("t"),
        }
    }

    /// Scripted replies, in order; records `(remote_id, if_match)` per call.
    struct Scripted {
        replies: Mutex<VecDeque<Result<HistoryAccepted, PushError>>>,
        calls: Mutex<Vec<(String, u64)>>,
    }

    impl Scripted {
        fn new(replies: Vec<Result<HistoryAccepted, PushError>>) -> Self {
            Self {
                replies: Mutex::new(replies.into()),
                calls: Mutex::default(),
            }
        }
    }

    impl HiveHistoryApi for Scripted {
        fn put_history(
            &self,
            remote_id: &str,
            if_match: u64,
            _body: &ConversationData,
        ) -> BoxFuture<'static, Result<HistoryAccepted, PushError>> {
            self.calls
                .lock()
                .unwrap()
                .push((remote_id.to_string(), if_match));
            let reply = self.replies.lock().unwrap().pop_front().expect("a reply");
            Box::pin(async move { reply })
        }

        fn create(
            &self,
            _client_id: &str,
            _body: &ConversationData,
        ) -> BoxFuture<'static, Result<CreatedConversation, PushError>> {
            unreachable!("these tests push, they do not create")
        }
    }

    fn accepted(version: u64) -> Result<HistoryAccepted, PushError> {
        Ok(HistoryAccepted { version })
    }

    /// The account's copy, as [`sync_after_stale`] reads it.
    struct OneDetail(super::super::wire::ConversationDetail);

    impl super::super::api::HiveConversationsApi for OneDetail {
        fn changes_since(
            &self,
            _cursor: &str,
            _limit: usize,
        ) -> BoxFuture<'static, Result<super::super::wire::ChangesPage, super::super::api::ApiError>>
        {
            unreachable!("only the detail is read")
        }

        fn conversation(
            &self,
            _remote_id: &str,
        ) -> BoxFuture<
            'static,
            Result<super::super::wire::ConversationDetail, super::super::api::ApiError>,
        > {
            let detail = self.0.clone();
            Box::pin(async move { Ok(detail) })
        }

        fn resume(
            &self,
            _remote_id: &str,
        ) -> BoxFuture<'static, Result<(), super::super::api::ApiError>> {
            unreachable!("only the detail is read")
        }
    }

    fn held_by(device: &str, version: u64, title: &str) -> OneDetail {
        let mut detail: super::super::wire::ConversationDetail = serde_json::from_value(
            serde_json::json!({"id": "r-1", "title": title, "version": version}),
        )
        .unwrap();
        detail.placement = Some(Placement::Device {
            device_id: device.into(),
        });
        OneDetail(detail)
    }

    fn synced_here(version: u64) -> ConversationSync {
        ConversationSync::Synced(crate::models::conversation::SyncedConversation {
            server_url: "https://hive.example".into(),
            remote_id: "r-1".into(),
            version,
            placement: Placement::Device {
                device_id: "tui-1".into(),
            },
            last_synced_at: None,
        })
    }

    /// AGE-2122: after a 412 (a rename on the web), the holder reads the
    /// account's version and pushes again on it; another holder gets nothing.
    #[tokio::test]
    async fn after_a_412_the_holder_pushes_again_on_the_accounts_version() {
        let history = Scripted::new(vec![accepted(11)]);
        let updated = sync_after_stale(
            &history,
            &held_by("tui-1", 10, "Renamed on the web"),
            "https://hive.example",
            "tui-1",
            "local-1",
            &synced_here(9),
            body("old title"),
        )
        .await
        .unwrap()
        .expect("pushed again");
        assert_eq!(
            *history.calls.lock().unwrap(),
            vec![("r-1".to_string(), 10)]
        );
        assert_eq!(updated.synced().unwrap().version, 11);

        let history = Scripted::new(vec![]);
        let none = sync_after_stale(
            &history,
            &held_by("desk-2", 10, "Taken over"),
            "https://hive.example",
            "tui-1",
            "local-1",
            &synced_here(9),
            body("old title"),
        )
        .await
        .unwrap();
        assert!(none.is_none(), "another device holds it now");
        assert!(history.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_push_sends_the_history_with_if_match_and_the_bearer() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/api/conversations/r-1/history"))
            .and(header("if-match", "3"))
            .and(header("authorization", "Bearer tok"))
            .and(body_partial_json(
                serde_json::json!({"title": "hello", "message_history": "[]"}),
            ))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"version": 4})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let api = HttpHiveHistoryApi::new(&server.uri(), Some(signed_in(&server)));

        let accepted = api.put_history("r-1", 3, &body("hello")).await.unwrap();
        assert_eq!(accepted.version, 4);
    }

    #[tokio::test]
    async fn status_codes_map_to_typed_errors() {
        let server = MockServer::start().await;
        for (id, status, reply) in [
            ("stale", 412, serde_json::json!({})),
            (
                "taken",
                409,
                serde_json::json!({
                    "error": "held_elsewhere",
                    "placement_device": {"id": "s-2", "name": "Laptop"},
                    "version": 9
                }),
            ),
            ("locked", 401, serde_json::json!({})),
            ("down", 503, serde_json::json!({})),
            ("bad", 422, serde_json::json!({})),
        ] {
            Mock::given(method("PUT"))
                .and(path(format!("/api/conversations/{id}/history")))
                .respond_with(ResponseTemplate::new(status).set_body_json(reply))
                .mount(&server)
                .await;
        }
        let api = HttpHiveHistoryApi::new(&server.uri(), None);
        let put = |id: &'static str| {
            let api = api.clone();
            async move { api.put_history(id, 1, &body("t")).await.unwrap_err() }
        };

        assert!(matches!(put("stale").await, PushError::Stale));
        match put("taken").await {
            PushError::HeldElsewhere(held) => {
                assert_eq!(held.continued_on(), "Continued on Laptop")
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(put("locked").await, PushError::SignedOut));
        assert!(matches!(put("down").await, PushError::Offline(_)));
        assert!(matches!(put("bad").await, PushError::Rejected(_)));
    }

    #[tokio::test]
    async fn a_local_only_conversation_makes_no_request() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;
        let api = HttpHiveHistoryApi::new(&server.uri(), None);
        let mut queue = PushQueue::default();
        queue.enqueue("local");

        let sync = ConversationSync::LocalOnly;
        drain(
            &api,
            &mut queue,
            |_| PushJob::for_device(&sync, "this-device", || body("t")),
            |_, _| panic!("a local-only conversation has no outcome"),
        )
        .await;

        assert!(queue.is_empty());
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[test]
    fn only_the_device_that_holds_a_conversation_pushes_it() {
        let mine = device_sync(2, "this-device");
        assert_eq!(
            PushJob::for_device(&mine, "this-device", || body("t"))
                .unwrap()
                .version,
            2
        );
        assert!(PushJob::for_device(&mine, "other", || body("t")).is_none());
        let cloud = ConversationSync::cloud("http://localhost", "r-1", None);
        assert!(PushJob::for_device(&cloud, "this-device", || body("t")).is_none());
    }

    #[tokio::test]
    async fn a_stale_push_asks_for_a_pull_and_is_never_retried_blind() {
        let api = Scripted::new(vec![Err(PushError::Stale), accepted(6)]);
        let mut queue = PushQueue::default();
        queue.enqueue("c1");
        let mut events = Vec::new();

        drain(&api, &mut queue, |_| Some(job(4)), |_, e| events.push(e)).await;
        assert_eq!(
            events,
            vec![PushEvent::NeedsPull {
                local_id: "c1".into()
            }]
        );
        assert!(queue.is_empty(), "no overwrite before the pull");

        // The owner pulled (version is now 5) and still holds it: push again.
        queue.enqueue("c1");
        drain(&api, &mut queue, |_| Some(job(5)), |_, e| events.push(e)).await;
        assert_eq!(
            events[1],
            PushEvent::Pushed {
                local_id: "c1".into(),
                version: 6
            }
        );
        assert_eq!(
            *api.calls.lock().unwrap(),
            vec![("r-1".into(), 4), ("r-1".into(), 5)]
        );
    }

    #[tokio::test]
    async fn a_conflict_stops_pushing_that_conversation_and_names_the_holder() {
        let held = ConflictBody {
            placement_device: Some(crate::services::hive_sync::PlacementDevice {
                id: Some("s-2".into()),
                name: Some("Laptop".into()),
            }),
            ..ConflictBody::default()
        };
        let api = Scripted::new(vec![Err(PushError::HeldElsewhere(Box::new(held.clone())))]);
        let mut queue = PushQueue::default();
        queue.enqueue("c1");
        let mut events = Vec::new();

        drain(&api, &mut queue, |_| Some(job(1)), |_, e| events.push(e)).await;
        assert_eq!(
            events,
            vec![PushEvent::HeldElsewhere {
                local_id: "c1".into(),
                held_by: held
            }]
        );

        // The next turn asks again; nothing is queued, nothing is sent.
        queue.enqueue("c1");
        assert!(queue.is_empty());
        assert!(queue.is_held("c1"));
        // Taking it back releases it.
        queue.release("c1");
        queue.enqueue("c1");
        assert_eq!(queue.len(), 1);
    }

    #[tokio::test]
    async fn offline_pushes_wait_and_go_out_in_order_one_per_conversation() {
        let api = Scripted::new(vec![
            Err(PushError::Offline("down".into())),
            accepted(2),
            accepted(2),
            accepted(2),
        ]);
        let mut queue = PushQueue::default();
        for id in ["a", "b", "a", "c", "b"] {
            queue.enqueue(id);
        }
        assert_eq!(queue.len(), 3, "one push per conversation");
        let mut events = Vec::new();

        drain(&api, &mut queue, |_| Some(job(1)), |_, e| events.push(e)).await;
        assert_eq!(events, vec![PushEvent::Offline]);
        assert_eq!(queue.len(), 3, "the failed push is still first in line");

        events.clear();
        drain(&api, &mut queue, |_| Some(job(1)), |_, e| events.push(e)).await;
        let pushed: Vec<_> = events
            .iter()
            .map(|event| match event {
                PushEvent::Pushed { local_id, .. } => local_id.as_str(),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(pushed, ["a", "b", "c"]);
        assert_eq!(api.calls.lock().unwrap().len(), 4);
    }

    #[test]
    fn a_refused_push_is_dropped_not_queued_forever() {
        let mut queue = PushQueue::default();
        let event = queue.settle("c1", Err(PushError::Rejected("422".into())));
        assert!(matches!(event, PushEvent::Dropped { .. }));
        assert!(queue.is_empty());
    }

    /// HX-13: a create whose `client_id` was deleted on the account is a 410;
    /// it settles as `Deleted` with the server's message, and is not queued
    /// again.
    #[tokio::test]
    async fn a_create_for_a_conversation_deleted_on_the_account_is_deleted() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/conversations"))
            .respond_with(ResponseTemplate::new(410).set_body_json(serde_json::json!({
                "error": "deleted", "message": "This conversation was deleted."
            })))
            .mount(&server)
            .await;
        let api = HttpHiveHistoryApi::new(&server.uri(), None);
        let error = api.create("local-1", &body("t")).await.unwrap_err();
        assert!(matches!(&error, PushError::Deleted(m) if m == "This conversation was deleted."));

        let mut queue = PushQueue::default();
        let event = queue.settle("local-1", Err(error));
        assert_eq!(
            event,
            PushEvent::Deleted {
                local_id: "local-1".into(),
                message: "This conversation was deleted.".into()
            }
        );
        assert!(queue.is_empty());
    }

    #[tokio::test]
    async fn a_new_conversation_is_created_with_its_local_id_and_held_by_this_device() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/conversations"))
            .and(header("authorization", "Bearer tok"))
            .and(body_partial_json(serde_json::json!({
                "client_id": "local-1",
                "placement": "device",
                "conversation": {"title": "hello"}
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "r-9", "version": 1, "placement": "device",
                "placement_device": {"id": "s-2", "name": null}
            })))
            .expect(1)
            .mount(&server)
            .await;
        let api = HttpHiveHistoryApi::new(&server.uri(), Some(signed_in(&server)));

        let created = api.create("local-1", &body("hello")).await.unwrap();
        assert_eq!(created.id, "r-9");
        assert_eq!(created.version, 1);
    }

    #[test]
    fn the_servers_own_message_is_the_notice() {
        let held = ConflictBody {
            message: Some(
                "This conversation is running on Work laptop. Continue it here first.".into(),
            ),
            ..ConflictBody::default()
        };
        assert_eq!(
            held.notice(),
            "This conversation is running on Work laptop. Continue it here first."
        );
        assert_eq!(
            ConflictBody::default().notice(),
            "Continued on another device"
        );
    }

    /// A frontend with no queue: a local conversation is created on its first
    /// turn, pushed afterwards, and a local one past its first turn sends
    /// nothing.
    #[tokio::test]
    async fn without_a_queue_a_conversation_is_created_then_pushed_and_local_stays_local() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/conversations"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"id": "r-5", "version": 1})),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/api/conversations/r-5/history"))
            .and(header("if-match", "1"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"version": 2})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let api = HttpHiveHistoryApi::new(&server.uri(), None);
        let url = server.uri();

        let created = sync_after_turn(
            &api,
            &url,
            "s-2",
            "local",
            &ConversationSync::LocalOnly,
            true,
            body("t"),
        )
        .await
        .unwrap()
        .expect("the first turn creates it");
        let synced = created.synced().unwrap();
        assert_eq!(synced.remote_id, "r-5");
        assert_eq!(
            synced.placement,
            Placement::Device {
                device_id: "s-2".into()
            }
        );

        let pushed = sync_after_turn(&api, &url, "s-2", "local", &created, false, body("t"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(pushed.synced().unwrap().version, 2);

        let nothing = sync_after_turn(
            &api,
            &url,
            "s-2",
            "local",
            &ConversationSync::LocalOnly,
            false,
            body("t"),
        )
        .await
        .unwrap();
        assert!(nothing.is_none());
        // Another device's conversation is not ours to push.
        let theirs = sync_after_turn(&api, &url, "s-9", "local", &created, false, body("t"))
            .await
            .unwrap();
        assert!(theirs.is_none());
        assert_eq!(server.received_requests().await.unwrap().len(), 2);
    }
}
