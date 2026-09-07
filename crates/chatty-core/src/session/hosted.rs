//! The hosted half of a conversation's transport: a turn run by a
//! `chatty-server` instead of in this process (AGE-298).
//!
//! [`SessionEvent`] stays the single contract. The server's wire events are a
//! serialization of it (AGE-281), so this client's whole job is to turn one
//! SSE body back into the same event sequence a local
//! [`AgentSession`](super::AgentSession) would have emitted, in the same
//! order. Nothing here invents a vocabulary of its own, and nothing here
//! interprets an event — the owner applies them exactly as it applies a local
//! turn's, which is what keeps a hosted conversation's persisted row the same
//! shape as a local one's.
//!
//! # What this does not do
//!
//! Approvals and clarifications are answered by a *separate* POST while the
//! SSE body is still open; the server addresses them to the very stores its
//! agent was built with. That is why the turn future and the resolve calls
//! are independent here, mirroring the local session where the owner resolves
//! through the store handles rather than through the turn.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Result, bail};
use futures::StreamExt;

use super::event::SessionEvent;
use super::{TurnInput, TurnKind};
use crate::models::clarification_store::ClarificationAnswer;
use crate::services::{StreamError, StreamErrorKind, extract_user_text};

/// How long to wait for the server to accept a turn before giving up. The
/// stream itself is not bounded here: chatty-core's own stall watchdog ends a
/// turn that yields nothing, and it runs on the server side of a hosted turn.
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// A conversation whose turns run on a `chatty-server`.
///
/// Cheap to clone-ish and holds no turn state beyond the cancel flag: the
/// server owns the session, the stores and the history. What lives here is
/// only how to address it.
pub struct HostedSession {
    http: reqwest::Client,
    /// Base URL without a trailing slash.
    server_url: String,
    /// The conversation's id *on the server*, not the local one.
    remote_id: String,
    /// Armed for the duration of a turn, mirroring `AgentSession`. Set by
    /// [`cancel`](Self::cancel) so a caller that cancels between the POST and
    /// the first frame still stops the turn.
    cancel_flag: Option<Arc<AtomicBool>>,
    /// True from the moment a turn is handed out until its future finishes.
    ///
    /// Separate from `cancel_flag` because the two answer different questions:
    /// the cancel flag says "stop", and a turn that ended normally never sets
    /// it. Deriving "is a turn running" from the cancel flag would leave this
    /// session looking busy forever after its first turn, and refuse the
    /// second. The future clears it, which is why it is shared and not a
    /// `bool` — nothing holds `&mut self` by the time a turn ends.
    turn_active: Arc<AtomicBool>,
}

impl HostedSession {
    pub fn new(server_url: impl Into<String>, remote_id: impl Into<String>) -> Self {
        Self::with_client(reqwest::Client::new(), server_url, remote_id)
    }

    pub fn with_client(
        http: reqwest::Client,
        server_url: impl Into<String>,
        remote_id: impl Into<String>,
    ) -> Self {
        Self {
            http,
            server_url: normalize_base_url(server_url.into()),
            remote_id: remote_id.into(),
            cancel_flag: None,
            turn_active: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn server_url(&self) -> &str {
        &self.server_url
    }

    pub fn remote_id(&self) -> &str {
        &self.remote_id
    }

    pub fn is_turn_active(&self) -> bool {
        self.turn_active.load(Ordering::Relaxed)
    }

    /// Start a turn on the server.
    ///
    /// Returns the turn as a future, like the local session: the caller spawns
    /// it and receives every outcome through `emit`, including the failures
    /// that happen before the stream opens. `TurnStarted` … `TurnEnded` is
    /// this client's guarantee as much as the session's, so a turn the server
    /// refuses is still a well-formed pair with an `Error` between them.
    pub fn begin_turn<F: FnMut(SessionEvent)>(
        &mut self,
        input: TurnInput,
        emit: F,
    ) -> Result<impl Future<Output = ()> + use<F>> {
        self.begin_turn_with_flag(input, Arc::new(AtomicBool::new(false)), emit)
    }

    /// [`begin_turn`](Self::begin_turn) with the owner's own cancellation
    /// token, for an owner that registers the turn before it can start (the
    /// desktop's `StreamManager`).
    pub fn begin_turn_with_flag<F: FnMut(SessionEvent)>(
        &mut self,
        input: TurnInput,
        cancel_flag: Arc<AtomicBool>,
        mut emit: F,
    ) -> Result<impl Future<Output = ()> + use<F>> {
        if self.is_turn_active() {
            bail!("a turn is already running");
        }

        // Attachments and llm-only contents do not cross the wire: the server
        // has no artifact sink (AGE-281, "not done"), so sending the paths
        // would name files it cannot read. AGE-298's table says attachments do
        // not move, and this is where that is true rather than merely stated.
        let text = extract_user_text(&input.contents);
        if text.trim().is_empty() && input.kind != TurnKind::Regenerate {
            bail!("a hosted turn needs text; attachments do not cross the wire yet");
        }

        let body = serde_json::json!({
            "text": text,
            "kind": wire_turn_kind(input.kind),
        });
        let url = format!(
            "{}/api/conversations/{}/turns",
            self.server_url, self.remote_id
        );
        let http = self.http.clone();
        self.cancel_flag = Some(cancel_flag.clone());
        self.turn_active.store(true, Ordering::Relaxed);
        let turn_active = self.turn_active.clone();

        Ok(async move {
            emit(SessionEvent::TurnStarted);
            if let Err(error) = drive_turn(&http, &url, body, &cancel_flag, &mut emit).await {
                emit(SessionEvent::Error(error));
            }
            // Released before `TurnEnded`, so an owner that starts the next
            // turn from its finalize — which the todo protocol's follow-up
            // does — is not refused by a turn that has already finished.
            turn_active.store(false, Ordering::Relaxed);
            // The local session emits `TurnEnded` on every path, including the
            // ones where the stream never opened. A hosted turn that the
            // server never accepted must look the same to the owner, or the
            // frontend never finalizes and the user message dangles.
            emit(SessionEvent::TurnEnded);
        })
    }

    /// Stop the running turn. The POST is what actually stops the server's
    /// loop; the local flag stops this client draining frames for a turn the
    /// owner has already finalized.
    pub fn cancel(&self) -> impl Future<Output = ()> + use<> {
        if let Some(flag) = self.cancel_flag.as_ref() {
            flag.store(true, Ordering::Relaxed);
        }
        let http = self.http.clone();
        let url = format!(
            "{}/api/conversations/{}/cancel",
            self.server_url, self.remote_id
        );
        async move {
            if let Err(error) = http.post(&url).send().await {
                tracing::warn!(?error, "failed to cancel a hosted turn");
            }
        }
    }

    /// Answer an `ApprovalRequested` raised by the turn in flight.
    pub fn resolve_approval(&self, id: &str, approved: bool) -> impl Future<Output = ()> + use<> {
        let http = self.http.clone();
        let url = format!(
            "{}/api/conversations/{}/approvals/{}",
            self.server_url, self.remote_id, id
        );
        async move {
            let sent = http
                .post(&url)
                .json(&serde_json::json!({ "approved": approved }))
                .send()
                .await;
            if let Err(error) = sent {
                tracing::warn!(?error, "failed to resolve a hosted approval");
            }
        }
    }

    /// Answer a `ClarificationRequested` raised by the turn in flight.
    pub fn resolve_clarification(
        &self,
        id: &str,
        answers: Vec<ClarificationAnswer>,
    ) -> impl Future<Output = ()> + use<> {
        let http = self.http.clone();
        let url = format!(
            "{}/api/conversations/{}/clarifications/{}",
            self.server_url, self.remote_id, id
        );
        async move {
            let sent = http
                .post(&url)
                .json(&serde_json::json!({ "answers": answers }))
                .send()
                .await;
            if let Err(error) = sent {
                tracing::warn!(?error, "failed to answer a hosted clarification");
            }
        }
    }
}

/// POST the turn and drain its SSE body into `emit`.
///
/// Errors returned here become one `Error` event; `TurnEnded` is the caller's
/// responsibility either way. The [`StreamErrorKind`] is what the recovery
/// table reads (AGE-244 / D5), so a server we could not reach has to be
/// `Transport` and not `Other`: the two get different treatment on the
/// headless surface, and a hosted turn is exactly the case where the network
/// is the thing that failed.
async fn drive_turn<F: FnMut(SessionEvent)>(
    http: &reqwest::Client,
    url: &str,
    body: serde_json::Value,
    cancel_flag: &Arc<AtomicBool>,
    emit: &mut F,
) -> std::result::Result<(), StreamError> {
    let response = http
        .post(url)
        .timeout(CONNECT_TIMEOUT)
        .json(&body)
        .send()
        .await
        .map_err(|error| {
            StreamError::new(
                StreamErrorKind::Transport,
                format!("could not reach the server at {url}: {error}"),
            )
        })?;

    let status = response.status();
    if !status.is_success() {
        let detail = response.text().await.unwrap_or_default();
        // The server's status stands in for the provider's here: the client
        // cannot see past it, and 401/429 mean the same thing either way.
        let kind = match status.as_u16() {
            401 | 403 => StreamErrorKind::Auth,
            429 => StreamErrorKind::RateLimited,
            code => StreamErrorKind::ProviderStatus(code),
        };
        return Err(StreamError::new(
            kind,
            format!("the server refused the turn ({status}): {}", detail.trim()),
        ));
    }

    let mut frames = SseFrames::default();
    let mut body = response.bytes_stream();
    while let Some(chunk) = body.next().await {
        if cancel_flag.load(Ordering::Relaxed) {
            // The cancel POST has already gone out; stop draining rather than
            // replaying a turn the owner has finalized.
            break;
        }
        let chunk = chunk.map_err(|error| {
            StreamError::new(
                StreamErrorKind::Transport,
                format!("the server's response stream failed: {error}"),
            )
        })?;
        for (name, data) in frames.push(&chunk) {
            match decode_frame(&name, &data) {
                Ok(Some(event)) => emit(event),
                // A frame that carried no event is still reported: a hole the
                // client cannot see is worse than an error it can.
                Ok(None) => {}
                Err(error) => emit(SessionEvent::Error(StreamError::new(
                    StreamErrorKind::Other,
                    error,
                ))),
            }
        }
    }
    Ok(())
}

/// One `event:`/`data:` frame off the wire, as a [`SessionEvent`].
///
/// `Ok(None)` is a frame that carries no session event: the server's
/// `stream_error`, or a future variant this build does not know. Both are
/// reported to the caller rather than dropped.
fn decode_frame(name: &str, data: &str) -> std::result::Result<Option<SessionEvent>, String> {
    if name == "stream_error" {
        let message = serde_json::from_str::<serde_json::Value>(data)
            .ok()
            .and_then(|value| {
                value
                    .get("message")
                    .and_then(|m| m.as_str())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| "the server could not serialize an event".to_string());
        return Err(message);
    }
    match serde_json::from_str::<SessionEvent>(data) {
        Ok(event) => Ok(Some(event)),
        Err(error) => Err(format!("could not decode a {name} event: {error}")),
    }
}

/// An incremental SSE parser: bytes in, complete `(event, data)` pairs out.
///
/// Only the two fields the server sends are recognised. Frames are separated
/// by a blank line and a `data:` field may repeat, which is why this
/// accumulates rather than reading one line at a time.
#[derive(Default)]
struct SseFrames {
    buffer: String,
    event: Option<String>,
    data: Vec<String>,
}

impl SseFrames {
    fn push(&mut self, chunk: &[u8]) -> Vec<(String, String)> {
        self.buffer.push_str(&String::from_utf8_lossy(chunk));
        let mut out = Vec::new();

        while let Some(newline) = self.buffer.find('\n') {
            let line = self.buffer[..newline].trim_end_matches('\r').to_string();
            self.buffer.drain(..=newline);

            if line.is_empty() {
                if let Some(event) = self.event.take()
                    && !self.data.is_empty()
                {
                    out.push((event, self.data.join("\n")));
                }
                self.data.clear();
            } else if let Some(value) = line.strip_prefix("event:") {
                self.event = Some(value.trim().to_string());
            } else if let Some(value) = line.strip_prefix("data:") {
                self.data
                    .push(value.strip_prefix(' ').unwrap_or(value).to_string());
            }
            // `:` comments and any other field are ignored, per the SSE spec.
        }
        out
    }
}

/// The `kind` a [`TurnKind`] is called on the wire (AGE-281).
///
/// A client echoing a `FollowUp` back **must** say `protocol_follow_up`: a
/// `human` turn resets the server session's todo protocol and its recovery
/// budget, which is exactly what a follow-up must not do.
fn wire_turn_kind(kind: TurnKind) -> &'static str {
    match kind {
        TurnKind::Human => "human",
        TurnKind::ProtocolFollowUp => "protocol_follow_up",
        TurnKind::Regenerate => "regenerate",
    }
}

fn normalize_base_url(url: String) -> String {
    url.trim_end_matches('/').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drain(frames: &mut SseFrames, chunk: &str) -> Vec<(String, String)> {
        frames.push(chunk.as_bytes())
    }

    #[test]
    fn parses_one_frame_per_event() {
        let mut frames = SseFrames::default();
        let out = drain(
            &mut frames,
            "event: turn_started\ndata: \"TurnStarted\"\n\nevent: text\ndata: {\"Text\":\"hi\"}\n\n",
        );
        assert_eq!(
            out,
            vec![
                ("turn_started".to_string(), "\"TurnStarted\"".to_string()),
                ("text".to_string(), "{\"Text\":\"hi\"}".to_string()),
            ]
        );
    }

    #[test]
    fn a_frame_split_across_chunks_is_reassembled() {
        let mut frames = SseFrames::default();
        assert!(drain(&mut frames, "event: te").is_empty());
        assert!(drain(&mut frames, "xt\ndata: {\"Text\":\"par").is_empty());
        let out = drain(&mut frames, "tial\"}\n\n");
        assert_eq!(
            out,
            vec![("text".to_string(), "{\"Text\":\"partial\"}".to_string())]
        );
    }

    #[test]
    fn a_repeated_data_field_is_joined_with_newlines() {
        let mut frames = SseFrames::default();
        let out = drain(&mut frames, "event: text\ndata: one\ndata: two\n\n");
        assert_eq!(out, vec![("text".to_string(), "one\ntwo".to_string())]);
    }

    #[test]
    fn decodes_every_wire_event_back_into_a_session_event() {
        // The contract is that the wire is a *serialization of* SessionEvent,
        // so a round trip through serde is the whole decoder.
        for event in [
            SessionEvent::TurnStarted,
            SessionEvent::Text("hi".into()),
            SessionEvent::ToolCallStarted {
                id: "call-1".into(),
                name: "read_file".into(),
            },
            SessionEvent::ApprovalRequested {
                id: "a-1".into(),
                command: "ls".into(),
                is_sandboxed: false,
            },
            SessionEvent::Cancelled,
            SessionEvent::TurnEnded,
            SessionEvent::FollowUp("next".into()),
        ] {
            let data = serde_json::to_string(&event).unwrap();
            let decoded = decode_frame("whatever", &data).unwrap();
            assert_eq!(
                serde_json::to_string(&decoded.unwrap()).unwrap(),
                data,
                "a {event:?} did not survive the wire"
            );
        }
    }

    #[test]
    fn a_stream_error_frame_becomes_an_error_not_a_dropped_event() {
        let err = decode_frame(
            "stream_error",
            r#"{"message":"failed to serialize a text event"}"#,
        )
        .unwrap_err();
        assert_eq!(err, "failed to serialize a text event");
    }

    #[test]
    fn an_undecodable_frame_is_reported_rather_than_skipped() {
        let err = decode_frame("text", "{not json").unwrap_err();
        assert!(err.starts_with("could not decode a text event"), "{err}");
    }

    /// A turn that ended releases the session, so the next one is accepted.
    ///
    /// Deriving "busy" from the cancel flag looks right and is not: a turn
    /// that ended normally never sets it, so the session would stay busy
    /// forever after its first turn and refuse every one after it.
    #[tokio::test]
    async fn a_finished_turn_releases_the_session_for_the_next_one() {
        // Nothing is listening on this port, so the turn fails at the POST —
        // which is the point: even the failing path has to release.
        let mut session = HostedSession::new("http://127.0.0.1:1", "c-1");
        assert!(!session.is_turn_active(), "a fresh session is idle");

        let turn = session
            .begin_turn(TurnInput::text("hello"), |_| {})
            .expect("the first turn starts");
        assert!(
            session.is_turn_active(),
            "the turn is running once handed out"
        );
        turn.await;

        assert!(!session.is_turn_active(), "a finished turn releases");
        assert!(
            session.begin_turn(TurnInput::text("again"), |_| {}).is_ok(),
            "the next turn must not be refused by the last one"
        );
    }

    /// Every turn is a well-formed `TurnStarted` … `TurnEnded` pair, including
    /// one that never reached the server: an owner that never sees `TurnEnded`
    /// never finalizes, and leaves the user's message dangling with no reply.
    #[tokio::test]
    async fn a_turn_the_server_never_answered_still_starts_and_ends() {
        let mut session = HostedSession::new("http://127.0.0.1:1", "c-1");
        let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let sink = seen.clone();
        session
            .begin_turn(TurnInput::text("hello"), move |event| {
                sink.borrow_mut().push(event_name_of(&event))
            })
            .expect("the turn starts")
            .await;

        let seen = seen.borrow();
        assert_eq!(seen.first().copied(), Some("turn_started"), "{seen:?}");
        assert_eq!(seen.last().copied(), Some("turn_ended"), "{seen:?}");
        assert!(
            seen.contains(&"error"),
            "the failure must be said: {seen:?}"
        );
    }

    fn event_name_of(event: &SessionEvent) -> &'static str {
        match event {
            SessionEvent::TurnStarted => "turn_started",
            SessionEvent::TurnEnded => "turn_ended",
            SessionEvent::Error(_) => "error",
            _ => "other",
        }
    }

    #[test]
    fn base_urls_normalize_so_routes_never_double_up_slashes() {
        let session = HostedSession::new("http://localhost:8081/", "c-1");
        assert_eq!(session.server_url(), "http://localhost:8081");
    }

    #[test]
    fn a_follow_up_is_sent_back_as_a_protocol_turn_not_a_human_one() {
        // A `human` turn resets the server's todo protocol and recovery
        // budget; echoing a FollowUp as one would uncap the budget.
        assert_eq!(
            wire_turn_kind(TurnKind::ProtocolFollowUp),
            "protocol_follow_up"
        );
        assert_eq!(wire_turn_kind(TurnKind::Human), "human");
        assert_eq!(wire_turn_kind(TurnKind::Regenerate), "regenerate");
    }
}
