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
//!
//! # Who the server thinks is asking
//!
//! A `chatty-server` answers every `/api` route with 401 unless the request
//! carries a bearer for a live login session (AGE-350, CX-0). The token is
//! the Hive sign-in's: the registry is the only issuer, and the server asks
//! it whose session a token belongs to. So a hosted conversation borrows the
//! process's [`HiveSession`] and every request here goes through
//! [`send_to_server`], which attaches its access token and retries once with
//! a refreshed one after a 401 (AGE-835). Signed out, the request goes out
//! bare and the 401 is reported as "sign in to Hive", not as a dead server.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result, bail};
use futures::StreamExt;
use hive_client::HiveSession;

use chatty_fabric::{ApprovalRequest, AskRequest};

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
    /// The Hive sign-in whose access token authenticates every request, or
    /// `None` to send them bare (signed out, or a test against a server
    /// that does not check).
    auth: Option<Arc<HiveSession>>,
    /// Where the frames that are not a [`SessionEvent`] go (HX-2). Logging
    /// unless the owner installs its own with [`set_frame_sink`](Self::set_frame_sink).
    frame_sink: FrameSink,
    /// Where this conversation's file map and downloaded files live
    /// (HX-15). A turn's attachments are uploaded through the Hive files API
    /// before the turn is sent; with no directory, a turn with attachments is
    /// refused rather than sent without them.
    files_dir: Option<PathBuf>,
}

/// The owner's side channel for [`HostedFrame`]s that are not session events.
pub type FrameSink = Arc<dyn Fn(HostedFrame) + Send + Sync>;

fn log_frame(frame: HostedFrame) {
    tracing::debug!(?frame, "hosted frame outside the session event stream");
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
            auth: None,
            frame_sink: Arc::new(log_frame),
            files_dir: None,
        }
    }

    /// Upload each turn's attachments through the files API, keeping the
    /// map of what was uploaded in `dir` (HX-15).
    pub fn set_files_dir(&mut self, dir: Option<PathBuf>) {
        self.files_dir = dir;
    }

    /// Hand the frames that are not session events (broker approvals and
    /// questions, connector sign-in, the attach marker) to `sink`.
    pub fn set_frame_sink(&mut self, sink: FrameSink) {
        self.frame_sink = sink;
    }

    /// Authenticate every request as the user signed in to `auth`.
    pub fn with_auth(mut self, auth: Option<Arc<HiveSession>>) -> Self {
        self.auth = auth;
        self
    }

    /// Replace the sign-in requests authenticate with: a later sign-in, or a
    /// sign-out, applies to a conversation that is already hosted.
    pub fn set_auth(&mut self, auth: Option<Arc<HiveSession>>) {
        self.auth = auth;
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
        input: &TurnInput,
        emit: F,
    ) -> Result<impl Future<Output = ()> + use<F>> {
        self.begin_turn_with_flag(input, Arc::new(AtomicBool::new(false)), emit)
    }

    /// [`begin_turn`](Self::begin_turn) with the owner's own cancellation
    /// token, for an owner that registers the turn before it can start (the
    /// desktop's `StreamManager`).
    pub fn begin_turn_with_flag<F: FnMut(SessionEvent)>(
        &mut self,
        input: &TurnInput,
        cancel_flag: Arc<AtomicBool>,
        mut emit: F,
    ) -> Result<impl Future<Output = ()> + use<F>> {
        if self.is_turn_active() {
            bail!("a turn is already running");
        }

        // Attachments cross as files (HX-15), not as message content: each
        // is uploaded to the conversation before the turn is sent, and the
        // server renders it from the conversation's file list. llm-only
        // contents do not cross. A turn is refused when it has neither text
        // nor an attachment, or has attachments and nowhere to keep the map
        // of what was uploaded.
        let text = extract_user_text(&input.contents);
        if text.trim().is_empty()
            && input.attachments.is_empty()
            && input.kind != TurnKind::Regenerate
        {
            bail!("a hosted turn needs text or an attachment");
        }
        let uploads = if input.attachments.is_empty() || input.kind == TurnKind::Regenerate {
            None
        } else {
            let Some(dir) = self.files_dir.clone() else {
                bail!("attachments in a hosted conversation need a file cache to upload from");
            };
            Some((dir, input.attachments.clone()))
        };

        let body = serde_json::json!({
            "text": text,
            "kind": wire_turn_kind(input.kind),
        });
        let url = format!(
            "{}/api/conversations/{}/turns",
            self.server_url, self.remote_id
        );
        let http = self.http.clone();
        let auth = self.auth.clone();
        let frame_sink = self.frame_sink.clone();
        let remote_id = self.remote_id.clone();
        self.cancel_flag = Some(cancel_flag.clone());
        self.turn_active.store(true, Ordering::Relaxed);
        let turn_active = self.turn_active.clone();

        Ok(async move {
            emit(SessionEvent::TurnStarted);
            let uploaded = match uploads {
                Some((dir, paths)) => {
                    upload_for_turn(&http, auth.clone(), &url, &dir, &paths).await
                }
                None => Ok(()),
            };
            // The server's stream carries its own `TurnStarted` and
            // `TurnEnded` (the wire is a serialization of `SessionEvent`), so
            // forwarding both and adding ours would hand the owner two of each
            // and finalize the turn twice. The client's `TurnStarted` is
            // already out; the server's `TurnEnded` is the one, emitted where
            // the server put it so a `FollowUp` after it stays after it. Ours
            // is only for a turn the server never ended.
            let mut ended = false;
            let sent = {
                let mut forward = |event: SessionEvent| match event {
                    SessionEvent::TurnStarted => {}
                    SessionEvent::TurnEnded => {
                        if !ended {
                            ended = true;
                            turn_active.store(false, Ordering::Relaxed);
                            emit(SessionEvent::TurnEnded);
                        }
                    }
                    other => emit(other),
                };
                match uploaded {
                    Ok(()) => {
                        drive_turn(
                            &http,
                            auth.as_deref(),
                            &url,
                            &remote_id,
                            body,
                            &cancel_flag,
                            &mut forward,
                            &frame_sink,
                        )
                        .await
                    }
                    Err(error) => Err(error),
                }
            };
            if let Err(error) = sent {
                if ended {
                    tracing::warn!(target: LOG_TARGET, remote_id, %error, "a hosted turn's stream failed after it ended");
                } else {
                    // The kind only: a refusal's message quotes the server's
                    // body, which the refusal's own line has the code of.
                    tracing::warn!(target: LOG_TARGET, remote_id, kind = ?error.kind, "a hosted turn failed");
                    emit(SessionEvent::Error(error));
                }
            }
            if !ended {
                // Released before `TurnEnded`, so an owner that starts the next
                // turn from its finalize — which the todo protocol's follow-up
                // does — is not refused by a turn that has already finished.
                turn_active.store(false, Ordering::Relaxed);
                // The local session emits `TurnEnded` on every path, including
                // the ones where the stream never opened. A hosted turn that
                // the server never accepted must look the same to the owner,
                // or the frontend never finalizes and the user message
                // dangles.
                emit(SessionEvent::TurnEnded);
            }
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
        let auth = self.auth.clone();
        let url = format!(
            "{}/api/conversations/{}/cancel",
            self.server_url, self.remote_id
        );
        let remote_id = self.remote_id.clone();
        async move {
            match send_to_server(auth.as_deref(), &url, || http.post(&url)).await {
                Ok(response) if !response.status().is_success() => {
                    log_refused("a turn's cancel", &remote_id, response).await;
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(target: LOG_TARGET, remote_id, error = format!("{error:#}"), "failed to cancel a hosted turn");
                }
            }
        }
    }

    /// Answer an `ApprovalRequested` raised by the turn in flight.
    pub fn resolve_approval(&self, id: &str, approved: bool) -> impl Future<Output = ()> + use<> {
        let http = self.http.clone();
        let auth = self.auth.clone();
        let url = format!(
            "{}/api/conversations/{}/approvals/{}",
            self.server_url, self.remote_id, id
        );
        let remote_id = self.remote_id.clone();
        async move {
            let body = serde_json::json!({ "approved": approved });
            match send_to_server(auth.as_deref(), &url, || http.post(&url).json(&body)).await {
                Ok(response) if !response.status().is_success() => {
                    log_refused("an approval", &remote_id, response).await;
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(target: LOG_TARGET, remote_id, error = format!("{error:#}"), "failed to resolve a hosted approval");
                }
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
        let auth = self.auth.clone();
        let url = format!(
            "{}/api/conversations/{}/clarifications/{}",
            self.server_url, self.remote_id, id
        );
        let remote_id = self.remote_id.clone();
        async move {
            let body = serde_json::json!({ "answers": answers });
            match send_to_server(auth.as_deref(), &url, || http.post(&url).json(&body)).await {
                Ok(response) if !response.status().is_success() => {
                    log_refused("a clarification answer", &remote_id, response).await;
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(target: LOG_TARGET, remote_id, error = format!("{error:#}"), "failed to answer a hosted clarification");
                }
            }
        }
    }

    /// Join the turn that is already running on the server — one started
    /// from the web, another device, or this one before it lost its
    /// connection (HX-10).
    ///
    /// The server replays the frames the turn has produced so far and then
    /// follows it live, so this is the same `TurnStarted` … `TurnEnded` pair
    /// [`begin_turn`](Self::begin_turn) gives, with two differences. The
    /// server's own `TurnStarted`/`TurnEnded` frames are swallowed and
    /// replaced by this client's, so the pair is exactly one even across a
    /// reconnect. And whatever a reconnect replays that was already emitted
    /// is skipped: text by characters, every other event by count (the
    /// server compacts text deltas but never reorders or merges anything
    /// else), so a dropped connection that comes back shows no repeated
    /// output.
    ///
    /// A connection that drops or goes quiet for [`AttachTiming::idle`] is
    /// re-opened after a short wait, a handful of times, before the turn is
    /// reported lost. `accept_gap` says whether to follow a turn whose
    /// earliest frames the server no longer holds (`dropped > 0`): without it
    /// that replay cannot be trusted, so this ends at once with
    /// [`AttachOutcome::NeedsDetail`] and the owner fetches the conversation
    /// instead (and may attach again with `accept_gap`).
    pub fn attach<F: FnMut(SessionEvent)>(
        &mut self,
        cancel_flag: Arc<AtomicBool>,
        accept_gap: bool,
        emit: F,
    ) -> Result<impl Future<Output = AttachOutcome> + use<F>> {
        self.attach_with_timing(cancel_flag, accept_gap, AttachTiming::default(), emit)
    }

    fn attach_with_timing<F: FnMut(SessionEvent)>(
        &mut self,
        cancel_flag: Arc<AtomicBool>,
        accept_gap: bool,
        timing: AttachTiming,
        mut emit: F,
    ) -> Result<impl Future<Output = AttachOutcome> + use<F>> {
        if self.is_turn_active() {
            bail!("a turn is already running");
        }
        let url = format!(
            "{}/api/conversations/{}/turn/events",
            self.server_url, self.remote_id
        );
        let http = self.http.clone();
        let auth = self.auth.clone();
        let frame_sink = self.frame_sink.clone();
        let remote_id = self.remote_id.clone();
        self.cancel_flag = Some(cancel_flag.clone());
        self.turn_active.store(true, Ordering::Relaxed);
        let turn_active = self.turn_active.clone();

        Ok(async move {
            emit(SessionEvent::TurnStarted);
            let outcome = drive_attach(
                &http,
                auth.as_deref(),
                &url,
                &remote_id,
                &cancel_flag,
                accept_gap,
                timing,
                &mut emit,
                &frame_sink,
            )
            .await;
            turn_active.store(false, Ordering::Relaxed);
            emit(SessionEvent::TurnEnded);
            outcome
        })
    }

    /// The approvals and questions the conversation's run is waiting on
    /// (HX-11): what to show for an input raised before this client attached.
    pub fn fetch_inputs(&self) -> impl Future<Output = Result<Vec<PendingInput>>> + use<> {
        let http = self.http.clone();
        let auth = self.auth.clone();
        let url = format!(
            "{}/api/conversations/{}/inputs",
            self.server_url, self.remote_id
        );
        let remote_id = self.remote_id.clone();
        async move {
            let response = send_to_server(auth.as_deref(), &url, || http.get(&url)).await?;
            let status = response.status();
            if !status.is_success() {
                log_refused("the pending inputs", &remote_id, response).await;
                if status == reqwest::StatusCode::UNAUTHORIZED {
                    bail!(SIGN_IN_TO_HIVE);
                }
                bail!("the server answered {status} for {url}");
            }
            let wire: Vec<PendingInputWire> = response
                .json()
                .await
                .with_context(|| format!("unexpected reply from {url}"))?;
            Ok(wire.into_iter().map(PendingInput::from).collect())
        }
    }

    /// Answer an input the run is waiting on, with the nonce it came with.
    /// A `409` means the input was settled already, here or elsewhere: that is
    /// an outcome, not a failure.
    pub fn answer_input(
        &self,
        id: &str,
        nonce: &str,
        answer: InputAnswer,
    ) -> impl Future<Output = AnswerOutcome> + use<> {
        let http = self.http.clone();
        let auth = self.auth.clone();
        let url = format!(
            "{}/api/conversations/{}/inputs/{}",
            self.server_url, self.remote_id, id
        );
        let body = match answer {
            InputAnswer::Approval(approved) => {
                serde_json::json!({ "nonce": nonce, "approved": approved })
            }
            InputAnswer::Answers(answers) => {
                let answers: Vec<chatty_fabric::Answer> = answers
                    .into_iter()
                    .map(|answer| chatty_fabric::Answer {
                        id: answer.id,
                        answer: answer.answer,
                        custom: answer.custom,
                    })
                    .collect();
                serde_json::json!({ "nonce": nonce, "answers": answers })
            }
        };
        let remote_id = self.remote_id.clone();
        async move {
            match send_to_server(auth.as_deref(), &url, || http.post(&url).json(&body)).await {
                Ok(response) if response.status().is_success() => AnswerOutcome::Accepted,
                Ok(response) if response.status() == reqwest::StatusCode::CONFLICT => {
                    AnswerOutcome::Stale
                }
                Ok(response) => {
                    let status = response.status();
                    log_refused("an input's answer", &remote_id, response).await;
                    if status == reqwest::StatusCode::UNAUTHORIZED {
                        AnswerOutcome::Failed(SIGN_IN_TO_HIVE.to_string())
                    } else {
                        AnswerOutcome::Failed(format!("the server answered {status}"))
                    }
                }
                Err(error) => {
                    tracing::warn!(target: LOG_TARGET, remote_id, error = format!("{error:#}"), "failed to answer a hosted input");
                    AnswerOutcome::Failed(format!("{error:#}"))
                }
            }
        }
    }
}

/// How an [`attach`](HostedSession::attach) ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachOutcome {
    /// The turn finished (or the owner cancelled). The server has the whole
    /// conversation now.
    Ended,
    /// No turn was running on the server (`204`): it finished before this
    /// client got there. Fetch the conversation to see how it ended.
    NotRunning,
    /// The server already has as many viewers attached as it allows (`429`).
    Busy,
    /// The server no longer holds the start of the turn, so its replay would
    /// leave a hole: fetch the conversation, then attach with `accept_gap`.
    NeedsDetail,
    /// The connection could not be kept; an `Error` event said so.
    Lost,
}

/// The waits [`HostedSession::attach`] uses. Separate so a test need not sit
/// through them.
#[derive(Debug, Clone, Copy)]
struct AttachTiming {
    /// A connection that delivers nothing for this long is replaced. The
    /// stream is otherwise unbounded, and a laptop that slept leaves a
    /// connection that never errors.
    idle: std::time::Duration,
    /// The wait before reconnect number one; each next one doubles it.
    backoff: std::time::Duration,
    /// Reconnects in a row that delivered nothing before giving up.
    retries: u32,
}

impl Default for AttachTiming {
    fn default() -> Self {
        Self {
            idle: std::time::Duration::from_secs(60),
            backoff: std::time::Duration::from_secs(1),
            retries: 5,
        }
    }
}

/// An approval or question the server's run is waiting on.
///
/// `Debug` leaves the nonce out: it authorizes the answer, so it must not
/// reach a log line.
#[derive(Clone, PartialEq)]
pub struct PendingInput {
    pub id: String,
    /// What the answer has to carry.
    pub nonce: String,
    pub request: BrokerRequest,
}

impl std::fmt::Debug for PendingInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingInput")
            .field("id", &self.id)
            .field("nonce", &REDACTED)
            .field("request", &self.request)
            .finish()
    }
}

/// What a `Debug` of a [`HostedFrame`] or [`PendingInput`] shows for a nonce.
const REDACTED: &str = "<redacted>";

/// The answer to a [`PendingInput`].
#[derive(Debug, Clone, PartialEq)]
pub enum InputAnswer {
    Approval(bool),
    Answers(Vec<ClarificationAnswer>),
}

/// What the server made of an [`InputAnswer`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnswerOutcome {
    Accepted,
    /// `409`: already answered or withdrawn, here or on another device.
    Stale,
    Failed(String),
}

/// `GET …/inputs` rows: the request is externally tagged, so the kind is the
/// key.
#[derive(serde::Deserialize)]
struct PendingInputWire {
    id: String,
    nonce: String,
    request: PendingRequestWire,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum PendingRequestWire {
    Approval(ApprovalRequest),
    Question(AskRequest),
}

impl From<PendingInputWire> for PendingInput {
    fn from(wire: PendingInputWire) -> Self {
        PendingInput {
            id: wire.id,
            nonce: wire.nonce,
            request: match wire.request {
                PendingRequestWire::Approval(request) => BrokerRequest::Approval(request),
                PendingRequestWire::Question(request) => BrokerRequest::Question(request),
            },
        }
    }
}

/// Upload a turn's attachments to the conversation the turn URL names, so
/// the server has them when the turn starts (HX-15).
async fn upload_for_turn(
    http: &reqwest::Client,
    auth: Option<Arc<HiveSession>>,
    turn_url: &str,
    dir: &Path,
    paths: &[PathBuf],
) -> std::result::Result<(), StreamError> {
    // `<server>/api/conversations/<id>/turns`
    let Some((base, remote_id)) = turn_url
        .strip_suffix("/turns")
        .and_then(|rest| rest.rsplit_once("/api/conversations/"))
    else {
        return Err(StreamError::new(
            StreamErrorKind::Transport,
            "the conversation's address is not one the files API can use",
        ));
    };
    let api = crate::services::hive_files::HttpHiveFilesApi::with_client(http.clone(), base, auth);
    crate::services::hive_files::upload_attachments(&api, remote_id, paths, dir)
        .await
        .map(|_| ())
        .map_err(|error| {
            let kind = match error {
                crate::services::hive_files::FilesError::SignedOut => StreamErrorKind::Auth,
                _ => StreamErrorKind::Transport,
            };
            tracing::warn!(target: LOG_TARGET, remote_id, %error, "Could not upload a hosted turn's attachments");
            StreamError::new(kind, format!("could not upload the attachments: {error}"))
        })
}

/// POST the turn and drain its SSE body into `emit`.
///
/// Errors returned here become one `Error` event; `TurnEnded` is the caller's
/// responsibility either way. The [`StreamErrorKind`] is what the recovery
/// table reads (AGE-244 / D5), so a server we could not reach has to be
/// `Transport` and not `Other`: the two get different treatment on the
/// headless surface, and a hosted turn is exactly the case where the network
/// is the thing that failed.
#[allow(clippy::too_many_arguments)]
async fn drive_turn<F: FnMut(SessionEvent)>(
    http: &reqwest::Client,
    auth: Option<&HiveSession>,
    url: &str,
    remote_id: &str,
    body: serde_json::Value,
    cancel_flag: &Arc<AtomicBool>,
    emit: &mut F,
    frame_sink: &FrameSink,
) -> std::result::Result<(), StreamError> {
    let response = send_to_server(auth, url, || {
        http.post(url).timeout(CONNECT_TIMEOUT).json(&body)
    })
    .await
    .map_err(|error| {
        tracing::warn!(target: LOG_TARGET, remote_id, error = format!("{error:#}"), "Could not send a hosted turn");
        StreamError::new(StreamErrorKind::Transport, format!("{error:#}"))
    })?;

    let status = response.status();
    if !status.is_success() {
        let detail = response.text().await.unwrap_or_default();
        tracing::warn!(
            target: LOG_TARGET,
            remote_id,
            status = status.as_u16(),
            error_code = error_code(&detail).as_deref(),
            "The server refused a hosted turn"
        );
        // The server's status stands in for the provider's here: the client
        // cannot see past it, and 401/429 mean the same thing either way.
        // A spent budget is terminal (AGE-903): `402 cap_exceeded`.
        if let Some(refusal) =
            crate::services::budget_stop::endpoint_refusal(status.as_u16(), &detail)
        {
            return Err(StreamError::new(StreamErrorKind::BudgetExhausted, refusal));
        }
        let kind = match status.as_u16() {
            401 | 403 => StreamErrorKind::Auth,
            429 => StreamErrorKind::RateLimited,
            code => StreamErrorKind::ProviderStatus(code),
        };
        let message = if status == reqwest::StatusCode::UNAUTHORIZED {
            SIGN_IN_TO_HIVE.to_string()
        } else if let Some(held) = held_elsewhere_message(status, &detail) {
            // The server's own words, with the next step already in them.
            held
        } else {
            format!("the server refused the turn ({status}): {}", detail.trim())
        };
        return Err(StreamError::new(kind, message));
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
                Ok(HostedFrame::Session(event)) => emit(event),
                // A frame the server could not serialize is reported: a hole
                // the client cannot see is worse than an error it can.
                Ok(HostedFrame::StreamError(error)) | Err(error) => emit(SessionEvent::Error(
                    StreamError::new(StreamErrorKind::Other, error),
                )),
                Ok(HostedFrame::Unknown(name)) => {
                    tracing::debug!(%name, "skipping an unknown hosted frame");
                }
                Ok(frame) => frame_sink(frame),
            }
        }
    }
    Ok(())
}

/// Follow the server's replay-then-live stream of the running turn into
/// `emit`, re-opening it when it drops. See [`HostedSession::attach`].
#[allow(clippy::too_many_arguments)]
async fn drive_attach<F: FnMut(SessionEvent)>(
    http: &reqwest::Client,
    auth: Option<&HiveSession>,
    url: &str,
    remote_id: &str,
    cancel_flag: &Arc<AtomicBool>,
    accept_gap: bool,
    timing: AttachTiming,
    emit: &mut F,
    frame_sink: &FrameSink,
) -> AttachOutcome {
    // What the owner has been given, across every connection of this attach.
    let mut emitted_text = 0usize;
    let mut emitted_other = 0usize;
    let mut first_dropped: Option<u64> = None;
    let mut failures = 0u32;

    let lost = |emit: &mut F, message: String| {
        tracing::warn!(target: LOG_TARGET, remote_id, %message, "Gave up following a cloud turn");
        emit(SessionEvent::Error(StreamError::new(
            StreamErrorKind::Transport,
            message,
        )));
        AttachOutcome::Lost
    };

    loop {
        if cancel_flag.load(Ordering::Relaxed) {
            return AttachOutcome::Ended;
        }
        let sent = tokio::time::timeout(
            CONNECT_TIMEOUT,
            send_to_server(auth, url, || {
                http.get(url)
                    .header(reqwest::header::ACCEPT, "text/event-stream")
            }),
        )
        .await;
        let response = match sent {
            Ok(Ok(response)) => response,
            Ok(Err(error)) => {
                failures += 1;
                tracing::info!(target: LOG_TARGET, remote_id, attempt = failures, error = format!("{error:#}"), "Could not reach a cloud turn; retrying");
                if failures > timing.retries {
                    return lost(emit, format!("{error:#}"));
                }
                tokio::time::sleep(timing.backoff * 2u32.pow(failures - 1)).await;
                continue;
            }
            Err(_) => {
                failures += 1;
                tracing::info!(target: LOG_TARGET, remote_id, attempt = failures, "A cloud turn did not answer in time; retrying");
                if failures > timing.retries {
                    return lost(emit, "the server did not answer in time".to_string());
                }
                continue;
            }
        };

        let status = response.status();
        if status == reqwest::StatusCode::NO_CONTENT {
            // Never attached: it ended before we got here. Attached before:
            // it ended while we were reconnecting.
            return if first_dropped.is_some() {
                AttachOutcome::Ended
            } else {
                AttachOutcome::NotRunning
            };
        }
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            tracing::info!(target: LOG_TARGET, remote_id, status = 429, "Too many viewers on a cloud turn");
            emit(SessionEvent::Error(StreamError::new(
                StreamErrorKind::RateLimited,
                "This conversation is already open in several places. Close one, then try again."
                    .to_string(),
            )));
            return AttachOutcome::Busy;
        }
        if !status.is_success() {
            tracing::warn!(target: LOG_TARGET, remote_id, status = status.as_u16(), attempt = failures, "The server refused to share a cloud turn");
            let kind = if status == reqwest::StatusCode::UNAUTHORIZED
                || status == reqwest::StatusCode::FORBIDDEN
            {
                StreamErrorKind::Auth
            } else {
                StreamErrorKind::ProviderStatus(status.as_u16())
            };
            let message = if status == reqwest::StatusCode::UNAUTHORIZED {
                SIGN_IN_TO_HIVE.to_string()
            } else {
                format!("the server refused to share the running turn ({status})")
            };
            emit(SessionEvent::Error(StreamError::new(kind, message)));
            return AttachOutcome::Lost;
        }

        // What this connection has replayed so far, to line up with what the
        // owner already has.
        let mut seen_text = 0usize;
        let mut seen_other = 0usize;
        let mut frames = SseFrames::default();
        let mut body = response.bytes_stream();
        let mut delivered = false;
        let mut ended = false;
        loop {
            if cancel_flag.load(Ordering::Relaxed) {
                return AttachOutcome::Ended;
            }
            let chunk = match tokio::time::timeout(timing.idle, body.next()).await {
                Ok(Some(Ok(chunk))) => chunk,
                // Closed, failed or quiet: reconnect unless the turn ended.
                Ok(Some(Err(_))) | Ok(None) | Err(_) => break,
            };
            for (name, data) in frames.push(&chunk) {
                delivered = true;
                match decode_frame(&name, &data) {
                    Ok(HostedFrame::Attached {
                        started_at_ms,
                        dropped,
                    }) => {
                        match first_dropped {
                            None => {
                                if dropped > 0 && !accept_gap {
                                    return AttachOutcome::NeedsDetail;
                                }
                                first_dropped = Some(dropped);
                            }
                            // The buffer moved on while we were away: the
                            // replay no longer lines up with what was shown.
                            Some(first) if first != dropped => {
                                return lost(
                                    emit,
                                    "Lost track of the cloud run while reconnecting. Reopen the conversation to pick it up again."
                                        .to_string(),
                                );
                            }
                            Some(_) => {}
                        }
                        frame_sink(HostedFrame::Attached {
                            started_at_ms,
                            dropped,
                        });
                    }
                    Ok(HostedFrame::Session(SessionEvent::TurnStarted)) => {}
                    Ok(HostedFrame::Session(SessionEvent::TurnEnded)) => ended = true,
                    Ok(HostedFrame::Session(SessionEvent::Text(text))) => {
                        seen_text += text.chars().count();
                        let new = seen_text.saturating_sub(emitted_text);
                        if new > 0 {
                            let total = text.chars().count();
                            let fresh: String = text.chars().skip(total - new).collect();
                            emitted_text += new;
                            emit(SessionEvent::Text(fresh));
                        }
                    }
                    Ok(HostedFrame::Session(event)) => {
                        seen_other += 1;
                        if seen_other > emitted_other {
                            emitted_other = seen_other;
                            emit(event);
                        }
                    }
                    Ok(HostedFrame::StreamError(error)) | Err(error) => emit(SessionEvent::Error(
                        StreamError::new(StreamErrorKind::Other, error),
                    )),
                    Ok(HostedFrame::Unknown(name)) => {
                        tracing::debug!(%name, "skipping an unknown hosted frame");
                    }
                    Ok(frame) => frame_sink(frame),
                }
            }
            if ended {
                return AttachOutcome::Ended;
            }
        }

        // The connection ended without the turn ending.
        if delivered {
            failures = 0;
        } else {
            failures += 1;
        }
        tracing::info!(target: LOG_TARGET, remote_id, attempt = failures, delivered, "A cloud turn's stream closed before the turn ended; reconnecting");
        if failures > timing.retries {
            return lost(
                emit,
                "Lost the connection to the cloud run. Reopen the conversation to pick it up again."
                    .to_string(),
            );
        }
        tokio::time::sleep(timing.backoff * 2u32.pow(failures.saturating_sub(1))).await;
    }
}

/// Where a user signs in to a connector the cloud run needs (HX-11): the
/// Hive settings page. `None` unless `hive_url` is `http(s)`, since this is
/// opened through the system's opener.
pub fn connector_settings_url(hive_url: &str) -> Option<String> {
    let base = hive_url.trim().trim_end_matches('/');
    let lower = base.to_ascii_lowercase();
    (lower.starts_with("https://") || lower.starts_with("http://"))
        .then(|| format!("{base}/settings/connectors"))
}

/// What a 401 from a `chatty-server` means to the user: the server checks
/// the Hive sign-in, and this process has none it would accept.
pub const SIGN_IN_TO_HIVE: &str = "The server needs you signed in to Hive (Settings → Extensions → \
     Hive, against the registry this server uses). Sign in, then try again.";

/// Send one request to a `chatty-server` as the user signed in to `auth`.
///
/// Every hosted request goes through here, so none can forget the bearer.
/// The URL must be `https://`, or `http://` to a loopback or private-LAN
/// address ([`hive_client::ensure_secure_url`]): the request carries the
/// user's Hive token and the conversation's history, and neither may cross
/// a network in clear text. The 401 retry with a refreshed token is
/// [`hive_client::send_authed`]'s, shared with the registry client, so a
/// hosted turn and a registry call never race each other into the
/// registry's refresh-token reuse detection.
pub(crate) async fn send_to_server(
    auth: Option<&HiveSession>,
    url: &str,
    build: impl Fn() -> reqwest::RequestBuilder,
) -> Result<reqwest::Response> {
    hive_client::ensure_secure_url(url).map_err(anyhow::Error::msg)?;
    hive_client::send_authed(auth, build)
        .await
        .with_context(|| format!("could not reach the server at {url}"))
}

/// The `tracing` target of every Hive sync, push, hand-off, reattach and
/// file-transfer failure line, so `RUST_LOG=chatty::hive=debug` (or one
/// grep for a `remote_id`) follows a conversation across all of them.
pub const LOG_TARGET: &str = "chatty::hive";

/// The `error` code of a Hive JSON error body (`{"error": "held_elsewhere",
/// "message": …}`), for a log line. Only the code: the message can quote
/// user content. `None` when the body is not such an object or the code is
/// not a short identifier.
pub fn error_code(body: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let code = value.get("error")?.as_str()?;
    (!code.is_empty()
        && code.len() <= 64
        && code
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'))
    .then(|| code.to_string())
}

/// Read a refused response's body once and log one line for it: the request
/// path (it names the conversation, `/api/conversations/<remote id>/…`), the
/// status and the server's error code — never the body itself. Returns the
/// status and the body for the caller to map.
pub(crate) async fn refused_body(
    what: &str,
    url: &str,
    response: reqwest::Response,
) -> (reqwest::StatusCode, String) {
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    let request = reqwest::Url::parse(url)
        .map(|url| url.path().to_string())
        .unwrap_or_default();
    tracing::warn!(
        target: LOG_TARGET,
        request,
        status = status.as_u16(),
        error_code = error_code(&body).as_deref(),
        "The server refused {what}"
    );
    (status, body)
}

/// Log a refused hosted request (any non-2xx answer) with the conversation's
/// remote id, the status and the server's error code, reading the body for
/// the code only.
async fn log_refused(what: &str, remote_id: &str, response: reqwest::Response) {
    let status = response.status().as_u16();
    let body = response.text().await.unwrap_or_default();
    tracing::warn!(
        target: LOG_TARGET,
        remote_id,
        status,
        error_code = error_code(&body).as_deref(),
        "The server refused {what}"
    );
}

/// One frame off a hosted turn's wire (HX-2).
///
/// Most are a [`SessionEvent`]; the rest are the server's own, which a client
/// that only knew session events used to turn into an error.
///
/// `Debug` shows a [`BrokerInput`](Self::BrokerInput)'s nonce as
/// `<redacted>`, so a frame can be logged as it is.
#[derive(Clone)]
pub enum HostedFrame {
    Session(SessionEvent),
    /// First frame of a re-attach: when the turn started and how many of its
    /// earliest frames the server's attach buffer no longer holds.
    Attached {
        started_at_ms: u64,
        dropped: u64,
    },
    /// An approval or question the root must answer, with the `nonce` its
    /// answer has to carry.
    BrokerInput {
        id: String,
        nonce: String,
        request: BrokerRequest,
    },
    /// The input `id` was answered (here or elsewhere) or withdrawn.
    BrokerInputSettled {
        id: String,
        outcome: InputOutcome,
    },
    ConnectorNeedsSignIn {
        connector: String,
    },
    /// Never on the wire: this client's answer to input `id` was refused as
    /// stale (`409`) — it was settled already, here or elsewhere.
    InputStale {
        id: String,
    },
    /// The server could not serialize an event; its message.
    StreamError(String),
    /// A frame name this build does not know. Skipped, never an error.
    Unknown(String),
}

impl std::fmt::Debug for HostedFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Session(event) => f.debug_tuple("Session").field(event).finish(),
            Self::Attached {
                started_at_ms,
                dropped,
            } => f
                .debug_struct("Attached")
                .field("started_at_ms", started_at_ms)
                .field("dropped", dropped)
                .finish(),
            Self::BrokerInput { id, request, .. } => f
                .debug_struct("BrokerInput")
                .field("id", id)
                .field("nonce", &REDACTED)
                .field("request", request)
                .finish(),
            Self::BrokerInputSettled { id, outcome } => f
                .debug_struct("BrokerInputSettled")
                .field("id", id)
                .field("outcome", outcome)
                .finish(),
            Self::ConnectorNeedsSignIn { connector } => f
                .debug_struct("ConnectorNeedsSignIn")
                .field("connector", connector)
                .finish(),
            Self::InputStale { id } => f.debug_struct("InputStale").field("id", id).finish(),
            Self::StreamError(message) => f.debug_tuple("StreamError").field(message).finish(),
            Self::Unknown(name) => f.debug_tuple("Unknown").field(name).finish(),
        }
    }
}

/// `409 {"error":"held_elsewhere","message":…}`: another device holds the
/// conversation. The message is written for the user and is shown as is.
fn held_elsewhere_message(status: reqwest::StatusCode, body: &str) -> Option<String> {
    if status != reqwest::StatusCode::CONFLICT {
        return None;
    }
    let body: serde_json::Value = serde_json::from_str(body).ok()?;
    if body.get("error")?.as_str()? != "held_elsewhere" {
        return None;
    }
    body.get("message")?.as_str().map(str::to_string)
}

/// What a [`HostedFrame::BrokerInput`] asks. The live frames carry the request
/// untagged (the frame name says which), so the kind is chosen at decode.
#[derive(Debug, Clone, PartialEq)]
pub enum BrokerRequest {
    Approval(ApprovalRequest),
    Question(AskRequest),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputOutcome {
    Approved,
    Denied,
    Answered,
    Withdrawn,
}

/// One `event:`/`data:` frame off the wire.
///
/// `Err` is a frame of a known kind that does not decode. A name this build
/// does not know, carrying well-formed JSON, is [`HostedFrame::Unknown`].
fn decode_frame(name: &str, data: &str) -> std::result::Result<HostedFrame, String> {
    use serde::Deserialize;
    use serde::de::DeserializeOwned;

    fn parse<T: DeserializeOwned>(name: &str, data: &str) -> std::result::Result<T, String> {
        serde_json::from_str(data)
            .map_err(|error| format!("could not decode a {name} event: {error}"))
    }
    #[derive(Deserialize)]
    struct Attached {
        started_at_ms: u64,
        dropped: u64,
    }
    #[derive(Deserialize)]
    struct Input<R> {
        id: String,
        nonce: String,
        request: R,
    }
    #[derive(Deserialize)]
    struct Settled {
        id: String,
        outcome: InputOutcome,
    }
    #[derive(Deserialize)]
    struct Connector {
        connector: String,
    }

    Ok(match name {
        "stream_error" => HostedFrame::StreamError(
            serde_json::from_str::<serde_json::Value>(data)
                .ok()
                .and_then(|value| value.get("message")?.as_str().map(str::to_string))
                .unwrap_or_else(|| "the server could not serialize an event".to_string()),
        ),
        "turn_attached" => {
            let a: Attached = parse(name, data)?;
            HostedFrame::Attached {
                started_at_ms: a.started_at_ms,
                dropped: a.dropped,
            }
        }
        "broker_approval" => {
            let i: Input<ApprovalRequest> = parse(name, data)?;
            HostedFrame::BrokerInput {
                id: i.id,
                nonce: i.nonce,
                request: BrokerRequest::Approval(i.request),
            }
        }
        "broker_question" => {
            let i: Input<AskRequest> = parse(name, data)?;
            HostedFrame::BrokerInput {
                id: i.id,
                nonce: i.nonce,
                request: BrokerRequest::Question(i.request),
            }
        }
        "broker_input_settled" => {
            let s: Settled = parse(name, data)?;
            HostedFrame::BrokerInputSettled {
                id: s.id,
                outcome: s.outcome,
            }
        }
        "connector_needs_sign_in" => {
            let c: Connector = parse(name, data)?;
            HostedFrame::ConnectorNeedsSignIn {
                connector: c.connector,
            }
        }
        _ => match serde_json::from_str::<SessionEvent>(data) {
            Ok(event) => HostedFrame::Session(event),
            // Well-formed JSON that is no session event is a frame from a
            // newer server; garbage is still reported.
            Err(_) if serde_json::from_str::<serde_json::Value>(data).is_ok() => {
                HostedFrame::Unknown(name.to_string())
            }
            Err(error) => return Err(format!("could not decode a {name} event: {error}")),
        },
    })
}

/// An incremental SSE parser: bytes in, complete `(event, data)` pairs out.
///
/// Only the two fields the server sends are recognised. Frames are separated
/// by a blank line and a `data:` field may repeat, which is why this
/// accumulates rather than reading one line at a time.
///
/// Bytes are buffered and only complete lines are decoded, so a multi-byte
/// character split across two chunks arrives intact.
#[derive(Default)]
struct SseFrames {
    buffer: Vec<u8>,
    event: Option<String>,
    data: Vec<String>,
}

impl SseFrames {
    fn push(&mut self, chunk: &[u8]) -> Vec<(String, String)> {
        self.buffer.extend_from_slice(chunk);
        let mut out = Vec::new();

        while let Some(newline) = self.buffer.iter().position(|&b| b == b'\n') {
            let raw: Vec<u8> = self.buffer.drain(..=newline).collect();
            let line = String::from_utf8_lossy(&raw[..newline]);
            let line = line.trim_end_matches('\r');

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
    fn a_multibyte_character_split_across_chunks_decodes_intact() {
        let mut frames = SseFrames::default();
        assert!(
            frames
                .push(b"event: text\ndata: {\"Text\":\"caf\xC3")
                .is_empty()
        );
        let out = frames.push(b"\xA9\"}\n\n");
        assert_eq!(
            out,
            vec![("text".to_string(), "{\"Text\":\"café\"}".to_string())]
        );
        let (name, data) = &out[0];
        match decode_frame(name, data) {
            Ok(HostedFrame::Session(SessionEvent::Text(t))) => assert_eq!(t, "café"),
            other => panic!("unexpected decode: {other:?}"),
        }
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
                detail: crate::models::execution_approval_store::ApprovalDetail::exec("ls"),
            },
            SessionEvent::Cancelled,
            SessionEvent::TurnEnded,
            SessionEvent::FollowUp("next".into()),
        ] {
            let data = serde_json::to_string(&event).unwrap();
            let Ok(HostedFrame::Session(decoded)) = decode_frame("whatever", &data) else {
                panic!("{event:?} did not decode as a session event");
            };
            assert_eq!(
                serde_json::to_string(&decoded).unwrap(),
                data,
                "a {event:?} did not survive the wire"
            );
        }
    }

    #[test]
    fn a_stream_error_frame_becomes_an_error_not_a_dropped_event() {
        let frame = decode_frame(
            "stream_error",
            r#"{"message":"failed to serialize a text event","code":"x"}"#,
        )
        .unwrap();
        assert!(
            matches!(&frame, HostedFrame::StreamError(m) if m == "failed to serialize a text event"),
            "{frame:?}"
        );
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
            .begin_turn(&TurnInput::text("hello"), |_| {})
            .expect("the first turn starts");
        assert!(
            session.is_turn_active(),
            "the turn is running once handed out"
        );
        turn.await;

        assert!(!session.is_turn_active(), "a finished turn releases");
        assert!(
            session
                .begin_turn(&TurnInput::text("again"), |_| {})
                .is_ok(),
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
            .begin_turn(&TurnInput::text("hello"), move |event| {
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

    fn approval() -> ApprovalRequest {
        ApprovalRequest {
            kind: chatty_fabric::ApprovalKind::Exec,
            command_or_path: "cargo test".into(),
            diff_stat: None,
            asker: None,
        }
    }

    fn question() -> AskRequest {
        AskRequest {
            questions: vec![chatty_fabric::Question {
                id: "q1".into(),
                question: "Which database?".into(),
                options: vec!["SQLite".into()],
            }],
            asker: None,
            origin: None,
        }
    }

    #[test]
    fn every_server_frame_kind_decodes_to_its_variant() {
        use serde_json::json;
        let cases = [
            (
                "turn_attached",
                json!({"started_at_ms": 1700000000000u64, "dropped": 3}),
                HostedFrame::Attached {
                    started_at_ms: 1700000000000,
                    dropped: 3,
                },
            ),
            (
                "broker_approval",
                json!({"id": "approval-1", "nonce": "n1", "request": approval()}),
                HostedFrame::BrokerInput {
                    id: "approval-1".into(),
                    nonce: "n1".into(),
                    request: BrokerRequest::Approval(approval()),
                },
            ),
            (
                "broker_question",
                json!({"id": "question-1", "nonce": "n2", "request": question()}),
                HostedFrame::BrokerInput {
                    id: "question-1".into(),
                    nonce: "n2".into(),
                    request: BrokerRequest::Question(question()),
                },
            ),
            (
                "broker_input_settled",
                json!({"id": "approval-1", "outcome": "withdrawn"}),
                HostedFrame::BrokerInputSettled {
                    id: "approval-1".into(),
                    outcome: InputOutcome::Withdrawn,
                },
            ),
            (
                "connector_needs_sign_in",
                json!({"connector": "github"}),
                HostedFrame::ConnectorNeedsSignIn {
                    connector: "github".into(),
                },
            ),
        ];
        for (name, data, expected) in cases {
            let decoded = decode_frame(name, &data.to_string()).unwrap();
            assert_eq!(format!("{decoded:?}"), format!("{expected:?}"), "{name}");
        }
    }

    #[test]
    fn error_code_reads_only_a_short_identifier() {
        assert_eq!(
            error_code(r#"{"error":"held_elsewhere","message":"Running on Work laptop"}"#)
                .as_deref(),
            Some("held_elsewhere")
        );
        assert_eq!(error_code("<html>oops</html>"), None);
        assert_eq!(error_code(r#"{"error":"the user wrote: hello"}"#), None);
        assert_eq!(error_code(r#"{"message":"no code"}"#), None);
    }

    /// The nonce authorizes an answer: a frame or pending input logged with
    /// `{:?}` (the default frame sink does) must not carry it.
    #[test]
    fn debug_output_never_shows_an_input_nonce() {
        use serde_json::json;
        let nonce = "nonce-5f1c0a77";
        let data = json!({"id": "approval-1", "nonce": nonce, "request": approval()});
        let frame = decode_frame("broker_approval", &data.to_string()).unwrap();
        let HostedFrame::BrokerInput { nonce: kept, .. } = &frame else {
            panic!("a broker input: {frame:?}");
        };
        assert_eq!(kept, nonce, "the nonce is still there to answer with");
        let shown = format!("{frame:?}");
        assert!(!shown.contains(nonce), "{shown}");
        assert!(shown.contains("approval-1"), "{shown}");

        let input = PendingInput {
            id: "approval-1".into(),
            nonce: nonce.into(),
            request: BrokerRequest::Approval(approval()),
        };
        let shown = format!("{input:?}");
        assert!(!shown.contains(nonce), "{shown}");
    }

    #[test]
    fn an_unknown_frame_is_skipped_and_the_turn_still_ends() {
        let frame = decode_frame("from_the_future", r#"{"anything":1}"#).unwrap();
        assert!(matches!(&frame, HostedFrame::Unknown(n) if n == "from_the_future"));
    }

    #[tokio::test]
    async fn non_session_frames_reach_the_sink_and_never_end_the_turn() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let body = "event: from_the_future\ndata: {\"x\":1}\n\n\
                        event: connector_needs_sign_in\ndata: {\"connector\":\"github\"}\n\n";
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\ncontent-length: {}\r\n\r\n{body}",
                body.len()
            );
        });
        let mut session = HostedSession::new(format!("http://{addr}"), "c-1");
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = seen.clone();
        session.set_frame_sink(Arc::new(move |frame| sink.lock().unwrap().push(frame)));
        let events = run_turn(&mut session).await;
        server.join().unwrap();

        let seen = seen.lock().unwrap();
        assert!(
            matches!(seen.as_slice(), [HostedFrame::ConnectorNeedsSignIn { connector }] if connector == "github"),
            "{seen:?}"
        );
        assert!(matches!(events.first(), Some(SessionEvent::TurnStarted)));
        assert!(matches!(events.last(), Some(SessionEvent::TurnEnded)));
        assert!(
            !events.iter().any(|e| matches!(e, SessionEvent::Error(_))),
            "{events:?}"
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

    fn token_pair(token: &str, refresh_token: &str) -> hive_client::TokenPair {
        hive_client::TokenPair {
            token: token.to_string(),
            refresh_token: refresh_token.to_string(),
            expires_at: chrono::Utc::now() + chrono::TimeDelta::hours(1),
        }
    }

    /// Run one turn against `session` and collect what it emitted.
    async fn run_turn(session: &mut HostedSession) -> Vec<SessionEvent> {
        let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let sink = seen.clone();
        session
            .begin_turn(&TurnInput::text("hello"), move |event| {
                sink.borrow_mut().push(event)
            })
            .expect("the turn starts")
            .await;
        seen.take()
    }

    /// AGE-835: a chatty-server answers every `/api` route with 401 unless
    /// the request carries the Hive sign-in's bearer. The turn sends it, and
    /// when the server rejects a stale one, the turn refreshes once through
    /// the registry and is sent again — the user never sees the 401.
    #[tokio::test]
    async fn a_hosted_turn_sends_the_hive_bearer_and_retries_once_after_a_refresh() {
        use wiremock::matchers::{header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let turns = "/api/conversations/c-1/turns";
        Mock::given(method("POST"))
            .and(path(turns))
            .and(header("authorization", "Bearer stale"))
            .respond_with(ResponseTemplate::new(401))
            .expect(1)
            .mount(&server)
            .await;
        let text = serde_json::to_string(&SessionEvent::Text("hosted hello".into())).unwrap();
        Mock::given(method("POST"))
            .and(path(turns))
            .and(header("authorization", "Bearer fresh"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(format!("event: text\ndata: {text}\n\n")),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/auth/refresh"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "token": "fresh",
                "refresh_token": "r-2",
                "expires_at": (chrono::Utc::now() + chrono::TimeDelta::hours(1)).to_rfc3339(),
            })))
            .expect(1)
            .mount(&server)
            .await;

        let hive = Arc::new(HiveSession::new(
            server.uri(),
            Some(token_pair("stale", "r-1")),
        ));
        let mut session = HostedSession::new(server.uri(), "c-1").with_auth(Some(hive));
        let events = run_turn(&mut session).await;

        assert!(
            events
                .iter()
                .any(|e| matches!(e, SessionEvent::Text(t) if t == "hosted hello")),
            "the authenticated turn's text must arrive: {events:?}"
        );
        assert!(
            !events.iter().any(|e| matches!(e, SessionEvent::Error(_))),
            "no error after the refresh: {events:?}"
        );
    }

    /// HX-15: a turn that is only an image is no longer refused. The image is
    /// uploaded to the conversation first, then the turn is sent.
    #[tokio::test]
    async fn a_hosted_turn_with_an_uploaded_image_is_accepted() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/files"))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "id": "f-1", "name": "pic.png", "media_type": "image/png",
                "head": {"id": "v-1", "size": 3}
            })))
            .expect(1)
            .mount(&server)
            .await;
        let text = serde_json::to_string(&SessionEvent::Text("I see it".into())).unwrap();
        Mock::given(method("POST"))
            .and(path("/api/conversations/c-1/turns"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(format!("event: text\ndata: {text}\n\n")),
            )
            .expect(1)
            .mount(&server)
            .await;
        let work = tempfile::tempdir().unwrap();
        let png = work.path().join("pic.png");
        std::fs::write(&png, [1u8, 2, 3]).unwrap();

        let mut session = HostedSession::new(server.uri(), "c-1");
        session.set_files_dir(Some(work.path().join("cache")));
        let mut input = TurnInput::text("");
        input.attachments = vec![png.clone()];
        let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let sink = seen.clone();
        session
            .begin_turn(&input, move |event| sink.borrow_mut().push(event))
            .expect("an uploaded image is a turn")
            .await;
        let events = seen.take();
        assert!(
            events
                .iter()
                .any(|e| matches!(e, SessionEvent::Text(t) if t == "I see it")),
            "{events:?}"
        );
        assert!(!events.iter().any(|e| matches!(e, SessionEvent::Error(_))));

        // Neither text nor attachment is still refused, and attachments with
        // nowhere to keep the upload map are refused rather than dropped.
        assert!(session.begin_turn(&TurnInput::text(""), |_| {}).is_err());
        let mut bare = HostedSession::new(server.uri(), "c-1");
        assert!(bare.begin_turn(&input, |_| {}).is_err());
    }

    /// Signed out, the server's 401 is reported as "sign in to Hive" with
    /// the `Auth` kind, not as an unreachable or broken server.
    #[tokio::test]
    async fn a_401_from_the_server_tells_the_user_to_sign_in_to_hive() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/conversations/c-1/turns"))
            .respond_with(ResponseTemplate::new(401).set_body_string(r#"{"error":"unauthorized"}"#))
            .mount(&server)
            .await;

        let mut session = HostedSession::new(server.uri(), "c-1");
        let events = run_turn(&mut session).await;
        let error = events
            .iter()
            .find_map(|e| match e {
                SessionEvent::Error(error) => Some(error),
                _ => None,
            })
            .expect("a refused turn reports an error");
        assert_eq!(error.kind, StreamErrorKind::Auth);
        assert_eq!(error.message, SIGN_IN_TO_HIVE);
    }

    /// The bearer and the history never go over plain http to a host off
    /// this machine's network.
    #[tokio::test]
    async fn a_plain_http_server_off_the_local_network_is_never_sent_the_bearer() {
        let hive = Arc::new(HiveSession::new(
            "http://127.0.0.1:1",
            Some(token_pair("secret", "r-1")),
        ));
        let mut session = HostedSession::new("http://93.184.216.34", "c-1").with_auth(Some(hive));
        let events = run_turn(&mut session).await;
        let error = events
            .iter()
            .find_map(|e| match e {
                SessionEvent::Error(error) => Some(error),
                _ => None,
            })
            .expect("the turn is refused");
        assert!(error.message.contains("plain http://"), "{}", error.message);
    }

    // ── attach (HX-10) and inputs (HX-11) ───────────────────────────────────

    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const EVENTS: &str = "/api/conversations/c-1/turn/events";

    fn sse_session(event: &SessionEvent) -> String {
        format!(
            "event: session\ndata: {}\n\n",
            serde_json::to_string(event).unwrap()
        )
    }

    fn sse_attached(dropped: u64) -> String {
        format!("event: turn_attached\ndata: {{\"started_at_ms\":1,\"dropped\":{dropped}}}\n\n")
    }

    fn sse(parts: &[String]) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_raw(parts.concat(), "text/event-stream")
    }

    fn fast() -> AttachTiming {
        AttachTiming {
            idle: std::time::Duration::from_millis(500),
            backoff: std::time::Duration::from_millis(1),
            retries: 3,
        }
    }

    async fn attach_once(
        server: &MockServer,
        accept_gap: bool,
    ) -> (AttachOutcome, Vec<SessionEvent>, HostedSession) {
        let mut session = HostedSession::new(server.uri(), "c-1");
        let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let sink = seen.clone();
        let outcome = session
            .attach_with_timing(
                Arc::new(AtomicBool::new(false)),
                accept_gap,
                fast(),
                move |e| sink.borrow_mut().push(e),
            )
            .expect("attach starts")
            .await;
        let events = seen.take();
        (outcome, events, session)
    }

    fn names(events: &[SessionEvent]) -> Vec<String> {
        events
            .iter()
            .map(|event| match event {
                SessionEvent::Text(t) => format!("text:{t}"),
                SessionEvent::ToolCallStarted { id, .. } => format!("tool:{id}"),
                SessionEvent::ToolCallResult { id, .. } => format!("result:{id}"),
                SessionEvent::TurnStarted => "start".into(),
                SessionEvent::TurnEnded => "end".into(),
                SessionEvent::Error(_) => "error".into(),
                other => format!("{other:?}"),
            })
            .collect()
    }

    fn tool(id: &str) -> SessionEvent {
        SessionEvent::ToolCallStarted {
            id: id.into(),
            name: "read_file".into(),
        }
    }

    #[tokio::test]
    async fn a_turn_for_a_conversation_another_device_holds_shows_the_servers_message() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/conversations/c-1/turns"))
            .respond_with(ResponseTemplate::new(409).set_body_json(serde_json::json!({
                "error": "held_elsewhere",
                "message": "This conversation is running on Work laptop. Continue it here first."
            })))
            .mount(&server)
            .await;
        let mut session = HostedSession::new(server.uri(), "c-1");
        let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let sink = seen.clone();
        session
            .begin_turn(&TurnInput::text("hello"), move |e| {
                sink.borrow_mut().push(e)
            })
            .expect("the turn starts")
            .await;
        let errors: Vec<String> = seen
            .borrow()
            .iter()
            .filter_map(|event| match event {
                SessionEvent::Error(error) => Some(error.to_string()),
                _ => None,
            })
            .collect();
        assert_eq!(
            errors,
            ["This conversation is running on Work laptop. Continue it here first."]
        );
    }

    #[tokio::test]
    async fn attach_replays_the_turn_as_one_started_ended_pair() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(EVENTS))
            .respond_with(sse(&[
                sse_attached(0),
                sse_session(&SessionEvent::TurnStarted),
                sse_session(&SessionEvent::Text("Hello".into())),
                sse_session(&tool("t1")),
                sse_session(&SessionEvent::TurnEnded),
            ]))
            .mount(&server)
            .await;
        let (outcome, events, session) = attach_once(&server, false).await;
        assert_eq!(outcome, AttachOutcome::Ended);
        assert_eq!(names(&events), ["start", "text:Hello", "tool:t1", "end"]);
        assert!(!session.is_turn_active(), "the attach released the session");
    }

    /// A connection that drops mid-turn is re-opened, and what the server
    /// replays that was already shown is skipped: text by characters (the
    /// server compacts deltas, so the split differs), everything else by count.
    #[tokio::test]
    async fn a_dropped_connection_reattaches_without_repeating_what_was_shown() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(EVENTS))
            .respond_with(sse(&[
                sse_attached(0),
                sse_session(&SessionEvent::TurnStarted),
                sse_session(&SessionEvent::Text("Hel".into())),
            ]))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(EVENTS))
            .respond_with(sse(&[
                sse_attached(0),
                sse_session(&SessionEvent::TurnStarted),
                sse_session(&SessionEvent::Text("Hello".into())),
                sse_session(&tool("t1")),
                sse_session(&SessionEvent::Text(", world".into())),
                sse_session(&SessionEvent::TurnEnded),
            ]))
            .mount(&server)
            .await;
        let (outcome, events, _) = attach_once(&server, false).await;
        assert_eq!(outcome, AttachOutcome::Ended);
        assert_eq!(
            names(&events),
            [
                "start",
                "text:Hel",
                "text:lo",
                "tool:t1",
                "text:, world",
                "end"
            ]
        );
    }

    #[tokio::test]
    async fn a_second_attach_replaying_everything_shows_each_event_once() {
        let server = MockServer::start().await;
        let first = [
            sse_attached(0),
            sse_session(&tool("t1")),
            sse_session(&SessionEvent::Text("a".into())),
        ];
        Mock::given(method("GET"))
            .and(path(EVENTS))
            .respond_with(sse(&first))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(EVENTS))
            .respond_with(sse(&[
                sse_attached(0),
                sse_session(&tool("t1")),
                sse_session(&SessionEvent::Text("a".into())),
                sse_session(&SessionEvent::ToolCallResult {
                    id: "t1".into(),
                    result: "ok".into(),
                }),
                sse_session(&SessionEvent::TurnEnded),
            ]))
            .mount(&server)
            .await;
        let (_, events, _) = attach_once(&server, false).await;
        assert_eq!(
            names(&events),
            ["start", "tool:t1", "text:a", "result:t1", "end"]
        );
    }

    /// `dropped > 0`: the replay is missing the turn's start, so it is not
    /// trusted — the owner is told to fetch the detail, and only a pair
    /// (nothing from the replay) reaches it.
    #[tokio::test]
    async fn a_replay_with_dropped_frames_asks_for_the_detail_instead() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(EVENTS))
            .respond_with(sse(&[
                sse_attached(7),
                sse_session(&SessionEvent::Text("late".into())),
                sse_session(&SessionEvent::TurnEnded),
            ]))
            .mount(&server)
            .await;
        let (outcome, events, _) = attach_once(&server, false).await;
        assert_eq!(outcome, AttachOutcome::NeedsDetail);
        assert_eq!(names(&events), ["start", "end"]);

        // After the detail, the gap is accepted and the live frames follow.
        let (outcome, events, _) = attach_once(&server, true).await;
        assert_eq!(outcome, AttachOutcome::Ended);
        assert_eq!(names(&events), ["start", "text:late", "end"]);
    }

    #[tokio::test]
    async fn attach_with_no_turn_running_or_too_many_viewers_still_ends_cleanly() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(EVENTS))
            .respond_with(ResponseTemplate::new(204))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(EVENTS))
            .respond_with(ResponseTemplate::new(429))
            .mount(&server)
            .await;
        let (outcome, events, _) = attach_once(&server, false).await;
        assert_eq!(outcome, AttachOutcome::NotRunning);
        assert_eq!(names(&events), ["start", "end"]);

        let (outcome, events, _) = attach_once(&server, false).await;
        assert_eq!(outcome, AttachOutcome::Busy);
        assert_eq!(names(&events), ["start", "error", "end"]);
    }

    #[tokio::test]
    async fn a_connection_that_never_comes_back_ends_the_turn_with_an_error() {
        // Nothing listens here, so every reconnect fails.
        let mut session = HostedSession::new("http://127.0.0.1:1", "c-1");
        let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let sink = seen.clone();
        let outcome = session
            .attach_with_timing(Arc::new(AtomicBool::new(false)), false, fast(), move |e| {
                sink.borrow_mut().push(e)
            })
            .unwrap()
            .await;
        assert_eq!(outcome, AttachOutcome::Lost);
        assert_eq!(names(&seen.borrow()), ["start", "error", "end"]);
    }

    #[tokio::test]
    async fn attach_is_refused_while_a_turn_is_running() {
        let mut session = HostedSession::new("http://127.0.0.1:1", "c-1");
        let _turn = session.begin_turn(&TurnInput::text("hi"), |_| {}).unwrap();
        assert!(
            session
                .attach(Arc::new(AtomicBool::new(false)), false, |_| {})
                .is_err()
        );
    }

    #[tokio::test]
    async fn pending_inputs_decode_by_their_tag_and_an_answer_carries_its_nonce() {
        let server = MockServer::start().await;
        // Generated per run: the point is that the answer echoes the nonce it
        // was given, whatever it is.
        let n1 = uuid::Uuid::new_v4().to_string();
        let n2 = uuid::Uuid::new_v4().to_string();
        Mock::given(method("GET"))
            .and(path("/api/conversations/c-1/inputs"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": "approval-1", "nonce": n1, "request": {"approval": approval()}},
                {"id": "question-2", "nonce": n2, "request": {"question": question()}},
            ])))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/conversations/c-1/inputs/approval-1"))
            .and(body_json(
                serde_json::json!({"nonce": n1, "approved": true}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/conversations/c-1/inputs/question-2"))
            .and(body_json(serde_json::json!({
                "nonce": n2,
                "answers": [{"id": "q1", "answer": "SQLite", "custom": false}]
            })))
            .respond_with(ResponseTemplate::new(409))
            .expect(1)
            .mount(&server)
            .await;

        let session = HostedSession::new(server.uri(), "c-1");
        let inputs = session.fetch_inputs().await.unwrap();
        assert_eq!(inputs.len(), 2);
        assert_eq!(inputs[0].request, BrokerRequest::Approval(approval()));
        assert_eq!(inputs[1].nonce, n2);
        assert_eq!(inputs[1].request, BrokerRequest::Question(question()));

        assert_eq!(
            session
                .answer_input("approval-1", &n1, InputAnswer::Approval(true))
                .await,
            AnswerOutcome::Accepted
        );
        // 409: settled elsewhere — an outcome, not a failure.
        let answers = vec![ClarificationAnswer {
            id: "q1".into(),
            answer: "SQLite".into(),
            custom: false,
        }];
        assert_eq!(
            session
                .answer_input("question-2", &n2, InputAnswer::Answers(answers))
                .await,
            AnswerOutcome::Stale
        );
    }

    #[test]
    fn the_connector_card_opens_hives_connector_settings_over_http_or_https_only() {
        assert_eq!(
            connector_settings_url("https://hive.example/").as_deref(),
            Some("https://hive.example/settings/connectors")
        );
        assert_eq!(
            connector_settings_url("http://localhost:8080").as_deref(),
            Some("http://localhost:8080/settings/connectors")
        );
        assert_eq!(connector_settings_url("file:///etc/passwd"), None);
        assert_eq!(connector_settings_url("javascript:alert(1)"), None);
        assert_eq!(connector_settings_url(""), None);
    }
}
