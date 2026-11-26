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

use super::wire::{HeldBy, HistoryAccepted};
use crate::models::conversation::{ConversationSync, Placement};
use crate::repositories::ConversationData;
use crate::repositories::conversation_repository::BoxFuture;
use crate::session::hosted::{SIGN_IN_TO_HIVE, send_to_server};

/// Why a push did not land.
#[derive(Debug, thiserror::Error)]
pub enum PushError {
    /// 412: the server's history moved on since `If-Match`. Pull, never
    /// overwrite.
    #[error("the server has a newer version of the conversation")]
    Stale,
    /// 409: another device holds the conversation now.
    #[error("another device holds the conversation")]
    HeldElsewhere(HeldBy),
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
            .map_err(|error| PushError::Offline(format!("{error:#}")))?;
            match response.status() {
                status if status.is_success() => response
                    .json::<HistoryAccepted>()
                    .await
                    .map_err(|error| PushError::Rejected(format!("unexpected reply: {error}"))),
                reqwest::StatusCode::PRECONDITION_FAILED => Err(PushError::Stale),
                reqwest::StatusCode::CONFLICT => Err(PushError::HeldElsewhere(
                    response.json::<HeldBy>().await.unwrap_or_default(),
                )),
                reqwest::StatusCode::UNAUTHORIZED => Err(PushError::SignedOut),
                status if status.is_server_error() => {
                    Err(PushError::Offline(format!("the server answered {status}")))
                }
                status => Err(PushError::Rejected(format!("the server answered {status}"))),
            }
        })
    }
}

/// This device's id on the server: the login session the bearer belongs to
/// (HX-7), found as the `current` row of `GET /api/auth/sessions`. A
/// conversation is held by this device when its `placement_device.id` is this
/// id. Fetch it once per sign-in; it is never sent in a body.
pub async fn fetch_device_id(
    server_url: &str,
    auth: Option<&HiveSession>,
) -> Result<String, PushError> {
    let url = format!("{}/api/auth/sessions", server_url.trim_end_matches('/'));
    hive_client::ensure_secure_url(&url).map_err(PushError::Rejected)?;
    let http = reqwest::Client::new();
    let response = send_to_server(auth, &url, || http.get(&url))
        .await
        .map_err(|error| PushError::Offline(format!("{error:#}")))?;
    match response.status() {
        status if status.is_success() => {}
        reqwest::StatusCode::UNAUTHORIZED => return Err(PushError::SignedOut),
        status if status.is_server_error() => {
            return Err(PushError::Offline(format!("the server answered {status}")));
        }
        status => return Err(PushError::Rejected(format!("the server answered {status}"))),
    }
    let sessions: serde_json::Value = response
        .json()
        .await
        .map_err(|error| PushError::Rejected(format!("unexpected reply: {error}")))?;
    // A bare array, or the array under `sessions`.
    sessions
        .get("sessions")
        .unwrap_or(&sessions)
        .as_array()
        .into_iter()
        .flatten()
        .find(|row| row.get("current").and_then(|current| current.as_bool()) == Some(true))
        .and_then(|row| row.get("id").and_then(|id| id.as_str()))
        .map(str::to_string)
        .ok_or_else(|| PushError::Rejected("this sign-in has no current session".to_string()))
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
    HeldElsewhere { local_id: String, held_by: HeldBy },
    /// 401: nothing more is pushed until the user signs in. The push stays
    /// queued.
    SignedOut,
    /// The server could not be reached. The push stays queued, in place.
    Offline,
    /// The server refused it for good; it is dropped, and the next turn
    /// queues a fresh one.
    Dropped { local_id: String, reason: String },
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
                PushEvent::HeldElsewhere { local_id, held_by }
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
    }

    fn accepted(version: u64) -> Result<HistoryAccepted, PushError> {
        Ok(HistoryAccepted { version })
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
        let held = HeldBy {
            placement_device: Some(crate::services::hive_sync::PlacementDevice {
                id: Some("s-2".into()),
                name: Some("Laptop".into()),
            }),
            ..HeldBy::default()
        };
        let api = Scripted::new(vec![Err(PushError::HeldElsewhere(held.clone()))]);
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

    #[tokio::test]
    async fn this_device_is_the_current_login_session() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/auth/sessions"))
            .and(header("authorization", "Bearer tok"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": "s-1", "current": false, "device_name": "Work laptop"},
                {"id": "s-2", "current": true, "device_name": "This one"}
            ])))
            .mount(&server)
            .await;
        let auth = signed_in(&server);

        assert_eq!(
            fetch_device_id(&server.uri(), Some(&auth)).await.unwrap(),
            "s-2"
        );
    }

    #[tokio::test]
    async fn a_sign_in_with_no_current_session_has_no_device() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/auth/sessions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"sessions": [{"id": "s-1"}]})),
            )
            .mount(&server)
            .await;
        assert!(matches!(
            fetch_device_id(&server.uri(), None).await,
            Err(PushError::Rejected(_))
        ));
    }
}
