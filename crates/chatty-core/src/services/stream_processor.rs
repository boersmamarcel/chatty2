use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;
use futures::StreamExt;
use tokio::sync::mpsc;

use crate::services::llm_service::{ResponseStream, StreamChunk};
use crate::tools::invoke_agent_tool::{InvokeAgentProgress, InvokeAgentProgressSlot};

/// Outcome returned by [`StreamChunkHandler::on_chunk`] to control the stream loop.
pub enum ChunkAction {
    /// Continue processing the next chunk.
    Continue,
    /// Break out of the stream loop immediately.
    Break,
}

/// Why a follow-up prompt is being queued for after the current turn.
///
/// Shared by both frontends (AGE-242 / D3) so the cancel-or-not policy for a
/// queued follow-up can't drift between the two hand-rolled stream handlers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FollowUpReason {
    /// The todo protocol wants a plan (or a verification) before more work.
    TodoProtocol,
    /// `AgentLoopGuard` saw the agent repeating itself.
    LoopGuard,
}

/// Whether queuing this follow-up should also cancel the in-flight stream.
///
/// Only the loop guard's pivot should: it fires precisely because the agent is
/// going in circles, so letting the turn run on is the thing being prevented.
///
/// The todo-protocol nudge must not. Cancelling for it broke the stream loop
/// before `StreamChunk::Done`, so the turn's streamed text was discarded — the
/// billed-but-empty assistant message in AGE-151 — and the nudge was delivered
/// into a turn that had just been torn down. The nudge asks the agent to plan
/// before doing *more* work; it never needed the work already done thrown away.
pub fn follow_up_requires_cancel(reason: FollowUpReason) -> bool {
    match reason {
        FollowUpReason::TodoProtocol => false,
        FollowUpReason::LoopGuard => true,
    }
}

/// The kind of error that ended a stream, classified once from rig's typed
/// error so frontends react on `kind` instead of sniffing message text
/// (AGE-244 / D5).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum StreamErrorKind {
    /// The provider rejected credentials (HTTP 401/403).
    Auth,
    /// The provider rate-limited the request (HTTP 429).
    RateLimited,
    /// Any other non-2xx HTTP status the provider returned.
    ProviderStatus(u16),
    /// A connection-level failure with no HTTP status (timeout, reset, DNS, ...).
    Transport,
    /// The provider returned malformed JSON for a tool call.
    MalformedToolCall,
    /// The stall watchdog ended the turn (`STALL_TIMEOUT`).
    Stalled,
    /// The model's final completion carried no text and no tool call, even
    /// after the in-turn nudge (AGE-401). Silence is not an answer.
    EmptyCompletion,
    /// The turn was cancelled by the user.
    Cancelled,
    /// Anything else (unknown tool call, max turns, memory error, ...).
    Other,
}

/// A stream-ending error, classified once in core so every surface makes the
/// same recovery decision from `kind` instead of matching on `message`
/// (AGE-244 / D5). `message` stays around for display and logging.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct StreamError {
    pub kind: StreamErrorKind,
    pub message: String,
}

impl StreamError {
    pub fn new(kind: StreamErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for StreamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// Where a stream is running. The same `StreamErrorKind` is handled
/// differently by surface (AGE-244 / D5's policy table): headless retries
/// transport failures on a fixed schedule, interactive surfaces show the
/// error and let the human decide.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamSurface {
    Desktop,
    InteractiveTui,
    Headless,
}

/// What a surface should do about a stream-ending error of a given kind,
/// decided once so the desktop, interactive TUI and headless runner can't
/// drift (AGE-244 / D5).
#[derive(Clone, Debug, PartialEq)]
pub enum RecoveryAction {
    /// Retry after this delay. Surfaces without their own retry loop (the
    /// desktop's Auth case today just refreshes the token for the *next*
    /// request) may treat this as "attempt the local recovery action"
    /// rather than literally re-running the turn.
    Retry { after: std::time::Duration },
    /// Inject a follow-up prompt instead of ending the turn.
    Nudge,
    /// End the turn; the surface presents the error as-is.
    Stop,
}

/// Headless's existing retry budgets (`headless/recovery.rs`, pre-AGE-244),
/// now the policy's parameters for that surface instead of being duplicated
/// in `is_retryable_stream_error`/`recovery_attempt_limit_for_error`.
pub const HEADLESS_TRANSPORT_RETRY_ATTEMPTS: usize = 5;
pub const HEADLESS_MALFORMED_JSON_RETRY_ATTEMPTS: usize = 2;

/// Decide what to do about a stream-ending error, per the D5 policy table.
///
/// `attempt` is how many recovery attempts have already been made for this
/// same error kind on this turn (0 for the first occurrence).
pub fn decide_recovery(
    kind: StreamErrorKind,
    surface: StreamSurface,
    attempt: usize,
) -> RecoveryAction {
    match kind {
        // Refresh (Azure) and retry the turn once, same on every surface.
        StreamErrorKind::Auth => {
            if attempt == 0 {
                RecoveryAction::Retry {
                    after: std::time::Duration::ZERO,
                }
            } else {
                RecoveryAction::Stop
            }
        }
        // Headless retries on a fixed schedule (today's behaviour); the
        // desktop and interactive TUI show the error with no auto retry.
        StreamErrorKind::RateLimited
        | StreamErrorKind::ProviderStatus(_)
        | StreamErrorKind::Transport => match surface {
            StreamSurface::Headless if attempt < HEADLESS_TRANSPORT_RETRY_ATTEMPTS => {
                RecoveryAction::Retry {
                    after: std::time::Duration::from_secs(10 * (attempt as u64 + 1)),
                }
            }
            _ => RecoveryAction::Stop,
        },
        // One protocol nudge, same on every surface; headless counts it
        // against its own (smaller) recovery budget.
        StreamErrorKind::MalformedToolCall => {
            let limit = match surface {
                StreamSurface::Headless => HEADLESS_MALFORMED_JSON_RETRY_ATTEMPTS,
                StreamSurface::Desktop | StreamSurface::InteractiveTui => 1,
            };
            if attempt < limit {
                RecoveryAction::Nudge
            } else {
                RecoveryAction::Stop
            }
        }
        // Stalled: end the turn with the stall message (today). Cancelled:
        // finalize per D4, handled outside this path. EmptyCompletion: the
        // nudge already happened inside the turn (AGE-401); a second empty
        // answer is the error. Other: surface as today.
        StreamErrorKind::Stalled
        | StreamErrorKind::Cancelled
        | StreamErrorKind::EmptyCompletion
        | StreamErrorKind::Other => RecoveryAction::Stop,
    }
}

/// Trait for handling stream chunks and progress events.
///
/// Both the GPUI and TUI frontends implement this trait to receive stream
/// events through their respective UI update mechanisms (GPUI entity updates
/// vs. channel-based event dispatch).
///
/// `on_chunk` is synchronous: nothing a handler does has to wait on I/O. The
/// Azure Entra token used to be refreshed here after a mid-stream 401, but it
/// is attached per request now (`AzureAuthHttpClient`, AGE-245). No `Send`
/// bound: the desktop's handler holds an `AsyncApp`, which is deliberately not
/// `Send`.
pub trait StreamChunkHandler {
    /// Called once when the stream loop starts (before the first chunk).
    fn on_stream_started(&mut self);

    /// Called for each LLM stream chunk. Return [`ChunkAction::Break`] to stop.
    fn on_chunk(&mut self, chunk: Result<StreamChunk>) -> Result<ChunkAction>;

    /// Called for each sub-agent progress event from `invoke_agent`.
    fn on_progress(&mut self, progress: InvokeAgentProgress);

    /// Called when the stream loop exits due to cancellation.
    fn on_cancelled(&mut self);

    /// Called after the stream loop finishes (whether normally or via error/cancel).
    ///
    /// Called exactly once per loop, on every exit path — including a
    /// handler error returned from `on_chunk`, which the loop still reports
    /// through its own `Result` after draining progress and calling this.
    fn on_stream_ended(&mut self);
}

/// How often the stream loop wakes when the provider is yielding nothing.
///
/// Only bounds how quickly a cancellation or a stall is noticed. It is not a
/// poll of anything.
pub const STALL_TICK: std::time::Duration = std::time::Duration::from_secs(5);

/// How long a stream may yield nothing before the turn is ended as stalled.
///
/// Generous on purpose: a long tool call (a build, a large fetch) is silence
/// from the stream's point of view, and cutting a live turn short is worse
/// than showing "working" for another minute.
pub const STALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(180);

/// Reported as a stream error when the watchdog above fires.
pub const STALLED_STREAM_MESSAGE: &str = "The model stopped responding (no output for 3 minutes). The turn was ended \
     — send a message to continue.";

/// Install a fresh progress sender into the shared slot, returning the receiver.
///
/// Both frontends need to install a progress channel before entering the stream
/// loop so that sub-agent events are routed to the correct receiver.
pub fn install_progress_channel(
    slot: &InvokeAgentProgressSlot,
) -> mpsc::UnboundedReceiver<InvokeAgentProgress> {
    let (tx, rx) = mpsc::unbounded_channel();
    let mut guard = slot.lock();
    *guard = Some(tx);
    rx
}

/// Run the main stream processing loop.
///
/// This is the core loop shared between the GPUI and TUI frontends. It
/// performs a biased `tokio::select!` between sub-agent progress events
/// and LLM stream chunks, checking the cancellation flag at the top of
/// each iteration.
///
/// The [`StreamChunkHandler`] receives all events and decides how to
/// forward them (GPUI → StreamManager entity, TUI → AppEvent channel).
pub async fn run_stream_loop(
    stream: &mut ResponseStream,
    progress_rx: &mut mpsc::UnboundedReceiver<InvokeAgentProgress>,
    cancel_flag: &Arc<AtomicBool>,
    handler: &mut impl StreamChunkHandler,
) -> Result<()> {
    handler.on_stream_started();

    let mut last_activity = std::time::Instant::now();
    let mut loop_result: Result<()> = Ok(());

    loop {
        if cancel_flag.load(Ordering::Relaxed) {
            handler.on_cancelled();
            break;
        }

        tokio::select! {
            biased;

            Some(progress) = progress_rx.recv() => {
                last_activity = std::time::Instant::now();
                handler.on_progress(progress);
            }

            // Wake periodically even when the provider yields nothing.
            //
            // `cancel_flag` is only read at the top of the loop and
            // `stream.next()` has no timeout, so a provider or tool that stops
            // yielding parked this loop indefinitely while the UI still showed
            // the turn as running (AGE-188).
            _ = tokio::time::sleep(STALL_TICK) => {
                if cancel_flag.load(Ordering::Relaxed) {
                    handler.on_cancelled();
                    break;
                }
                if last_activity.elapsed() >= STALL_TIMEOUT {
                    tracing::warn!(
                        idle_secs = last_activity.elapsed().as_secs(),
                        "Stream produced nothing for too long; ending the turn as stalled"
                    );
                    // Capture rather than `?`: the turn must still reach
                    // `on_stream_ended` on this exit path (AGE-213).
                    if let Err(e) = handler.on_chunk(Ok(StreamChunk::Error(StreamError::new(
                        StreamErrorKind::Stalled,
                        STALLED_STREAM_MESSAGE,
                    )))) {
                        loop_result = Err(e);
                    }
                    break;
                }
            }

            chunk_result = stream.next() => {
                last_activity = std::time::Instant::now();
                match chunk_result {
                    Some(result) => {
                        // Capture rather than `?`, so a handler error still
                        // reaches `on_stream_ended` (AGE-213).
                        match handler.on_chunk(result) {
                            Ok(ChunkAction::Continue) => {}
                            Ok(ChunkAction::Break) => break,
                            Err(e) => {
                                loop_result = Err(e);
                                break;
                            }
                        }
                    }
                    None => break,
                }
            }
        }
    }

    // A sub-agent that finished just as the stream ended still has events
    // queued, and the loop stopped reading. Dropping them left the last line of
    // its progress row on screen forever, so drain before ending the turn.
    while let Ok(progress) = progress_rx.try_recv() {
        handler.on_progress(progress);
    }

    handler.on_stream_ended();
    loop_result
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;
    use std::sync::atomic::AtomicBool;

    // -------------------------------------------------------------------
    // Follow-up cancel policy (AGE-242 / D3)
    // -------------------------------------------------------------------

    #[test]
    fn todo_protocol_follow_up_does_not_require_cancel() {
        assert!(!follow_up_requires_cancel(FollowUpReason::TodoProtocol));
    }

    #[test]
    fn loop_guard_follow_up_requires_cancel() {
        assert!(follow_up_requires_cancel(FollowUpReason::LoopGuard));
    }

    // -------------------------------------------------------------------
    // Stall watchdog (AGE-188)
    // -------------------------------------------------------------------

    #[test]
    fn stall_watchdog_wakes_well_inside_its_timeout() {
        assert!(
            STALL_TICK < STALL_TIMEOUT,
            "the watchdog has to wake before it can fire"
        );
        assert!(
            STALL_TIMEOUT.as_secs() >= 60,
            "a long tool call is silence from the stream's point of view; \
             ending a live turn early is worse than showing 'working' longer"
        );
    }

    #[test]
    fn stalled_stream_message_says_what_happened_and_what_to_do() {
        assert!(STALLED_STREAM_MESSAGE.contains("stopped responding"));
        assert!(STALLED_STREAM_MESSAGE.contains("send a message"));
    }

    struct TestHandler {
        started: bool,
        ended: bool,
        cancelled: bool,
        chunks: Vec<StreamChunk>,
        progress_events: Vec<InvokeAgentProgress>,
    }

    impl TestHandler {
        fn new() -> Self {
            Self {
                started: false,
                ended: false,
                cancelled: false,
                chunks: Vec::new(),
                progress_events: Vec::new(),
            }
        }
    }

    impl StreamChunkHandler for TestHandler {
        fn on_stream_started(&mut self) {
            self.started = true;
        }

        fn on_chunk(&mut self, chunk: Result<StreamChunk>) -> Result<ChunkAction> {
            let chunk = chunk?;
            let is_done = matches!(chunk, StreamChunk::Done);
            let is_error = matches!(chunk, StreamChunk::Error(_));
            self.chunks.push(chunk);
            if is_done || is_error {
                Ok(ChunkAction::Break)
            } else {
                Ok(ChunkAction::Continue)
            }
        }

        fn on_progress(&mut self, progress: InvokeAgentProgress) {
            self.progress_events.push(progress);
        }

        fn on_cancelled(&mut self) {
            self.cancelled = true;
        }

        fn on_stream_ended(&mut self) {
            self.ended = true;
        }
    }

    #[tokio::test]
    async fn stream_loop_processes_text_and_done() {
        let chunks: Vec<Result<StreamChunk>> = vec![
            Ok(StreamChunk::Text("hello ".into())),
            Ok(StreamChunk::Text("world".into())),
            Ok(StreamChunk::Done),
        ];
        let mut stream: ResponseStream = Box::pin(futures::stream::iter(chunks));
        let (_, mut progress_rx) = mpsc::unbounded_channel();
        let cancel_flag = Arc::new(AtomicBool::new(false));

        let mut handler = TestHandler::new();
        run_stream_loop(&mut stream, &mut progress_rx, &cancel_flag, &mut handler)
            .await
            .unwrap();

        assert!(handler.started);
        assert!(handler.ended);
        assert!(!handler.cancelled);
        assert_eq!(handler.chunks.len(), 3);
        assert!(matches!(handler.chunks[0], StreamChunk::Text(ref t) if t == "hello "));
        assert!(matches!(handler.chunks[2], StreamChunk::Done));
    }

    #[tokio::test]
    async fn stream_loop_respects_cancellation() {
        // Stream that never ends
        let mut stream: ResponseStream =
            Box::pin(futures::stream::pending::<Result<StreamChunk>>());
        let (_, mut progress_rx) = mpsc::unbounded_channel();
        let cancel_flag = Arc::new(AtomicBool::new(true)); // Pre-cancelled

        let mut handler = TestHandler::new();
        run_stream_loop(&mut stream, &mut progress_rx, &cancel_flag, &mut handler)
            .await
            .unwrap();

        assert!(handler.started);
        assert!(handler.cancelled);
        assert!(handler.ended);
        assert!(handler.chunks.is_empty());
    }

    #[tokio::test]
    async fn install_progress_channel_replaces_sender() {
        let slot: InvokeAgentProgressSlot = Arc::new(Mutex::new(None));
        assert!(slot.lock().is_none());

        let _rx = install_progress_channel(&slot);
        assert!(slot.lock().is_some());
    }

    // -------------------------------------------------------------------
    // on_stream_ended runs on every exit path, including a handler error
    // (AGE-213 / finding A5)
    // -------------------------------------------------------------------

    struct ErroringHandler {
        started: bool,
        ended: bool,
        chunk_count: usize,
    }

    impl StreamChunkHandler for ErroringHandler {
        fn on_stream_started(&mut self) {
            self.started = true;
        }

        fn on_chunk(&mut self, chunk: Result<StreamChunk>) -> Result<ChunkAction> {
            chunk?;
            self.chunk_count += 1;
            if self.chunk_count == 2 {
                anyhow::bail!("handler exploded on the second chunk");
            }
            Ok(ChunkAction::Continue)
        }

        fn on_progress(&mut self, _progress: InvokeAgentProgress) {}

        fn on_cancelled(&mut self) {}

        fn on_stream_ended(&mut self) {
            self.ended = true;
        }
    }

    #[tokio::test]
    async fn handler_error_still_runs_on_stream_ended() {
        let chunks: Vec<Result<StreamChunk>> = vec![
            Ok(StreamChunk::Text("first".into())),
            Ok(StreamChunk::Text("second".into())),
            // Never reached: the handler errors on the second chunk above.
            Ok(StreamChunk::Text("third".into())),
        ];
        let mut stream: ResponseStream = Box::pin(futures::stream::iter(chunks));
        let (_, mut progress_rx) = mpsc::unbounded_channel();
        let cancel_flag = Arc::new(AtomicBool::new(false));

        let mut handler = ErroringHandler {
            started: false,
            ended: false,
            chunk_count: 0,
        };
        let outcome =
            run_stream_loop(&mut stream, &mut progress_rx, &cancel_flag, &mut handler).await;

        assert!(handler.started);
        assert!(
            handler.ended,
            "on_stream_ended must run even when the handler errors"
        );
        assert!(outcome.is_err(), "the handler's error must propagate");
    }
    // -------------------------------------------------------------------
    // Loop contract (AGE-191 / AGE-192)
    //
    // Both frontends now drive this loop, so its callback sequence is the
    // shared half of the behaviour their own goldens pin. Recorded here, in
    // chatty-core, with no frontend crate in the dependency graph.
    // -------------------------------------------------------------------

    /// Records the loop's calls, not a frontend's interpretation of them.
    struct RecordingHandler {
        calls: Vec<String>,
    }

    fn label(chunk: &StreamChunk) -> &'static str {
        match chunk {
            StreamChunk::Text(_) => "Text",
            StreamChunk::ToolCallStarted { .. } => "ToolCallStarted",
            StreamChunk::ToolCallInput { .. } => "ToolCallInput",
            StreamChunk::ToolCallResult { .. } => "ToolCallResult",
            StreamChunk::ToolCallError { .. } => "ToolCallError",
            StreamChunk::ApprovalRequested { .. } => "ApprovalRequested",
            StreamChunk::ApprovalResolved { .. } => "ApprovalResolved",
            StreamChunk::ClarificationRequested { .. } => "ClarificationRequested",
            StreamChunk::ApiCallUsage(_) => "ApiCallUsage",
            StreamChunk::TurnUsage(_) => "TokenUsage",
            StreamChunk::TurnMessages(_) => "TurnMessages",
            StreamChunk::Done => "Done",
            StreamChunk::Error(_) => "Error",
        }
    }

    impl StreamChunkHandler for RecordingHandler {
        fn on_stream_started(&mut self) {
            self.calls.push("on_stream_started".to_string());
        }

        fn on_chunk(&mut self, chunk: Result<StreamChunk>) -> Result<ChunkAction> {
            match chunk {
                Ok(chunk) => {
                    let name = label(&chunk);
                    // Terminate on the same chunks both frontends terminate on,
                    // so the recorded sequence reflects a real turn.
                    let action = if matches!(chunk, StreamChunk::Done | StreamChunk::Error(_)) {
                        ChunkAction::Break
                    } else {
                        ChunkAction::Continue
                    };
                    self.calls.push(format!(
                        "on_chunk(Ok({name})) -> {}",
                        match action {
                            ChunkAction::Break => "Break",
                            ChunkAction::Continue => "Continue",
                        }
                    ));
                    Ok(action)
                }
                Err(e) => {
                    self.calls
                        .push(format!("on_chunk(Err({:?})) -> Break", e.to_string()));
                    Ok(ChunkAction::Break)
                }
            }
        }

        fn on_progress(&mut self, progress: InvokeAgentProgress) {
            let name = match progress {
                InvokeAgentProgress::Started { .. } => "Started",
                InvokeAgentProgress::Text(_) => "Text",
                InvokeAgentProgress::Finished { .. } => "Finished",
            };
            self.calls.push(format!("on_progress({name})"));
        }

        fn on_cancelled(&mut self) {
            self.calls.push("on_cancelled".to_string());
        }

        fn on_stream_ended(&mut self) {
            self.calls.push("on_stream_ended".to_string());
        }
    }

    async fn record_loop(scenario: crate::services::stream_fixtures::Scenario) -> Vec<String> {
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let (progress_tx, mut progress_rx) = mpsc::unbounded_channel();
        for progress in scenario.progress {
            progress_tx.send(progress).expect("receiver is alive");
        }
        drop(progress_tx);

        let mut stream =
            crate::services::stream_fixtures::scripted_stream(scenario.items, cancel_flag.clone());
        let mut handler = RecordingHandler { calls: Vec::new() };

        let outcome =
            run_stream_loop(&mut stream, &mut progress_rx, &cancel_flag, &mut handler).await;
        handler.calls.push(match outcome {
            Ok(()) => "=> loop returned Ok".to_string(),
            Err(e) => format!("=> loop returned Err({:?})", e.to_string()),
        });
        handler.calls
    }

    #[tokio::test]
    async fn loop_callback_sequence_matches_goldens() {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("src/services/goldens/stream_loop");
        for scenario in crate::services::stream_fixtures::scenarios()
            .into_iter()
            .chain([crate::services::stream_fixtures::clarification_scenario()])
        {
            let name = scenario.name;
            let calls = record_loop(scenario).await;
            crate::services::stream_fixtures::assert_golden(&dir, name, &calls);
        }
    }

    /// The loop reads the stream until it is told to stop, so a sub-agent that
    /// finished as the turn ended still has events queued. They used to be
    /// dropped, leaving the desktop's progress row on a stale line.
    #[tokio::test]
    async fn trailing_progress_is_drained_before_the_stream_ends() {
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let (progress_tx, mut progress_rx) = mpsc::unbounded_channel();

        let chunks: Vec<Result<StreamChunk>> = vec![Ok(StreamChunk::Done)];
        let mut stream: ResponseStream = Box::pin(futures::stream::iter(chunks));

        // Queued but never reached by the loop: Done breaks on the first chunk.
        progress_tx
            .send(InvokeAgentProgress::Finished {
                success: true,
                result: Some("done".into()),
            })
            .expect("receiver is alive");

        let mut handler = RecordingHandler { calls: Vec::new() };
        // Break on Done before the biased progress branch can run.
        handler
            .on_chunk(Ok(StreamChunk::Done))
            .expect("handler does not fail");
        handler.calls.clear();

        run_stream_loop(&mut stream, &mut progress_rx, &cancel_flag, &mut handler)
            .await
            .expect("loop completes");

        let ended = handler
            .calls
            .iter()
            .position(|c| c == "on_stream_ended")
            .expect("the loop ends the stream");
        let drained = handler
            .calls
            .iter()
            .position(|c| c == "on_progress(Finished)")
            .expect("the queued progress event is drained, not dropped");
        assert!(
            drained < ended,
            "progress must be drained before the turn ends, got {:?}",
            handler.calls
        );
    }

    // -------------------------------------------------------------------
    // Recovery policy (AGE-244 / D5): every kind x surface combination.
    // -------------------------------------------------------------------

    const ALL_KINDS: [StreamErrorKind; 9] = [
        StreamErrorKind::Auth,
        StreamErrorKind::RateLimited,
        StreamErrorKind::ProviderStatus(503),
        StreamErrorKind::Transport,
        StreamErrorKind::MalformedToolCall,
        StreamErrorKind::Stalled,
        StreamErrorKind::Cancelled,
        StreamErrorKind::EmptyCompletion,
        StreamErrorKind::Other,
    ];
    const ALL_SURFACES: [StreamSurface; 3] = [
        StreamSurface::Desktop,
        StreamSurface::InteractiveTui,
        StreamSurface::Headless,
    ];

    #[test]
    fn policy_covers_every_kind_and_surface_without_panicking() {
        for kind in ALL_KINDS {
            for surface in ALL_SURFACES {
                // Exhaustiveness is the point of this test: every combination
                // must return *some* decision, not panic on an unhandled arm.
                let _ = decide_recovery(kind, surface, 0);
            }
        }
    }

    #[test]
    fn auth_retries_once_then_stops() {
        for surface in ALL_SURFACES {
            assert!(matches!(
                decide_recovery(StreamErrorKind::Auth, surface, 0),
                RecoveryAction::Retry { .. }
            ));
            assert_eq!(
                decide_recovery(StreamErrorKind::Auth, surface, 1),
                RecoveryAction::Stop
            );
        }
    }

    #[test]
    fn transport_class_retries_only_on_headless() {
        for kind in [
            StreamErrorKind::RateLimited,
            StreamErrorKind::ProviderStatus(500),
            StreamErrorKind::Transport,
        ] {
            assert_eq!(
                decide_recovery(kind, StreamSurface::Desktop, 0),
                RecoveryAction::Stop
            );
            assert_eq!(
                decide_recovery(kind, StreamSurface::InteractiveTui, 0),
                RecoveryAction::Stop
            );
            assert!(matches!(
                decide_recovery(kind, StreamSurface::Headless, 0),
                RecoveryAction::Retry { .. }
            ));
        }
    }

    #[test]
    fn headless_transport_retry_stops_after_its_budget() {
        let last_retry = HEADLESS_TRANSPORT_RETRY_ATTEMPTS - 1;
        assert!(matches!(
            decide_recovery(
                StreamErrorKind::Transport,
                StreamSurface::Headless,
                last_retry
            ),
            RecoveryAction::Retry { .. }
        ));
        assert_eq!(
            decide_recovery(
                StreamErrorKind::Transport,
                StreamSurface::Headless,
                HEADLESS_TRANSPORT_RETRY_ATTEMPTS
            ),
            RecoveryAction::Stop
        );
    }

    #[test]
    fn malformed_tool_call_nudges_once_on_interactive_surfaces() {
        for surface in [StreamSurface::Desktop, StreamSurface::InteractiveTui] {
            assert_eq!(
                decide_recovery(StreamErrorKind::MalformedToolCall, surface, 0),
                RecoveryAction::Nudge
            );
            assert_eq!(
                decide_recovery(StreamErrorKind::MalformedToolCall, surface, 1),
                RecoveryAction::Stop
            );
        }
    }

    #[test]
    fn malformed_tool_call_uses_the_smaller_headless_budget() {
        let last_retry = HEADLESS_MALFORMED_JSON_RETRY_ATTEMPTS - 1;
        assert_eq!(
            decide_recovery(
                StreamErrorKind::MalformedToolCall,
                StreamSurface::Headless,
                last_retry
            ),
            RecoveryAction::Nudge
        );
        assert_eq!(
            decide_recovery(
                StreamErrorKind::MalformedToolCall,
                StreamSurface::Headless,
                HEADLESS_MALFORMED_JSON_RETRY_ATTEMPTS
            ),
            RecoveryAction::Stop
        );
    }

    #[test]
    fn stalled_cancelled_empty_and_other_always_stop() {
        for kind in [
            StreamErrorKind::Stalled,
            StreamErrorKind::Cancelled,
            StreamErrorKind::EmptyCompletion,
            StreamErrorKind::Other,
        ] {
            for surface in ALL_SURFACES {
                assert_eq!(decide_recovery(kind, surface, 0), RecoveryAction::Stop);
            }
        }
    }
}
