use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;
use futures::StreamExt;
use tokio::sync::mpsc;

use crate::services::llm_service::{ResponseStream, StreamChunk};
use crate::services::shell_service::MAX_SHELL_CALL_TIMEOUT_SECONDS;
use crate::tools::invoke_agent_tool::{InvokeAgentProgress, InvokeAgentProgressSlot};

/// Outcome returned by [`StreamChunkHandler::on_chunk`] to control the stream loop.
pub enum ChunkAction {
    /// Continue processing the next chunk.
    Continue,
    /// Break out of the stream loop immediately.
    Break,
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
    /// The model called a tool name that isn't registered or isn't allowed
    /// this turn — usually a one-token alias mistake (AGE-497).
    UnknownToolCall,
    /// Anything else (max turns, memory error, ...).
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

/// How many times headless resumes a turn the stall watchdog ended before it
/// gives up. A briefly overloaded local server (vLLM, Ollama) goes quiet for
/// minutes and then recovers; with nobody there to "send a message to
/// continue", the run would otherwise end and lose its work. Each resume
/// already cost a full [`STALL_TIMEOUT`] of silence, so the budget is small.
pub const HEADLESS_STALL_RESUME_ATTEMPTS: usize = 2;

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
        StreamErrorKind::MalformedToolCall | StreamErrorKind::UnknownToolCall => {
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
        // Headless resumes a stalled turn right away (the silence was the
        // wait); interactive surfaces show the stall message and a human
        // decides whether to send "continue".
        StreamErrorKind::Stalled => match surface {
            StreamSurface::Headless if attempt < HEADLESS_STALL_RESUME_ATTEMPTS => {
                RecoveryAction::Retry {
                    after: std::time::Duration::ZERO,
                }
            }
            _ => RecoveryAction::Stop,
        },
        // Cancelled: finalize per D4, handled outside this path.
        // EmptyCompletion: the nudge already happened inside the turn
        // (AGE-401); a second empty answer is the error. Other: surface as
        // today.
        StreamErrorKind::Cancelled | StreamErrorKind::EmptyCompletion | StreamErrorKind::Other => {
            RecoveryAction::Stop
        }
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

/// How long a stream may yield nothing while a tool call it announced has not
/// returned yet.
///
/// A tool runs between its `ToolCallStarted` chunk and its result, and the
/// stream is silent the whole time. `shell_execute` lets the model ask for up
/// to [`MAX_SHELL_CALL_TIMEOUT_SECONDS`] per command, so the plain
/// [`STALL_TIMEOUT`] would end a live turn three minutes into a ten-minute
/// test run. The shell's own timeout still bounds the command; this only
/// keeps the watchdog from firing first.
pub const TOOL_STALL_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(MAX_SHELL_CALL_TIMEOUT_SECONDS as u64 + STALL_TIMEOUT.as_secs());

/// The watchdog's timing, so a test can run it in milliseconds; production
/// callers go through [`run_stream_loop`], which uses the constants above.
#[derive(Debug, Clone, Copy)]
struct StallPolicy {
    tick: std::time::Duration,
    timeout: std::time::Duration,
    /// The timeout while a tool call is running.
    tool_timeout: std::time::Duration,
}

impl StallPolicy {
    const DEFAULT: Self = Self {
        tick: STALL_TICK,
        timeout: STALL_TIMEOUT,
        tool_timeout: TOOL_STALL_TIMEOUT,
    };
}

/// Whether `chunk` changes what the watchdog is waiting on: `Some(true)` once
/// the model has handed off a tool call (the tool is running now), and
/// `Some(false)` once a tool result or new model output shows the tool is
/// done. Bookkeeping chunks (usage, input echoes) leave it as it was: rig
/// reports a call's usage after its tool calls but before they run.
fn tool_running_after(chunk: &StreamChunk) -> Option<bool> {
    match chunk {
        StreamChunk::ToolCallStarted { .. } => Some(true),
        StreamChunk::ToolCallResult { .. }
        | StreamChunk::ToolCallError { .. }
        | StreamChunk::Text(_)
        | StreamChunk::Reasoning(_)
        | StreamChunk::ToolCallDelta => Some(false),
        _ => None,
    }
}

/// Reported as a stream error when the watchdog above fires; `timeout` is
/// the one that fired ([`STALL_TIMEOUT`], or [`TOOL_STALL_TIMEOUT`] while a
/// tool call was running).
pub fn stalled_stream_message(timeout: std::time::Duration) -> String {
    format!(
        "The model stopped responding (no output for {}). The turn was ended \
         — send a message to continue.",
        describe_duration(timeout)
    )
}

/// "3 minutes", "13 minutes", "90 seconds": whole minutes when it divides.
fn describe_duration(duration: std::time::Duration) -> String {
    let secs = duration.as_secs();
    let (n, unit) = if secs >= 60 && secs.is_multiple_of(60) {
        (secs / 60, "minute")
    } else {
        (secs, "second")
    };
    let plural = if n == 1 { "" } else { "s" };
    format!("{n} {unit}{plural}")
}

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
    run_stream_loop_with(
        stream,
        progress_rx,
        cancel_flag,
        handler,
        StallPolicy::DEFAULT,
    )
    .await
}

async fn run_stream_loop_with(
    stream: &mut ResponseStream,
    progress_rx: &mut mpsc::UnboundedReceiver<InvokeAgentProgress>,
    cancel_flag: &Arc<AtomicBool>,
    handler: &mut impl StreamChunkHandler,
    stall: StallPolicy,
) -> Result<()> {
    handler.on_stream_started();

    let mut last_activity = std::time::Instant::now();
    let mut tool_running = false;
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
            _ = tokio::time::sleep(stall.tick) => {
                if cancel_flag.load(Ordering::Relaxed) {
                    handler.on_cancelled();
                    break;
                }
                let timeout = if tool_running { stall.tool_timeout } else { stall.timeout };
                if last_activity.elapsed() >= timeout {
                    tracing::warn!(
                        idle_secs = last_activity.elapsed().as_secs(),
                        "Stream produced nothing for too long; ending the turn as stalled"
                    );
                    // Capture rather than `?`: the turn must still reach
                    // `on_stream_ended` on this exit path (AGE-213).
                    if let Err(e) = handler.on_chunk(Ok(StreamChunk::Error(StreamError::new(
                        StreamErrorKind::Stalled,
                        stalled_stream_message(timeout),
                    )))) {
                        loop_result = Err(e);
                    }
                    break;
                }
            }

            // Any chunk counts, including `Reasoning` and `ToolCallDelta`,
            // which no frontend renders: a model thinking for minutes or
            // writing a long tool argument is busy, not stalled (AGE-453).
            chunk_result = stream.next() => {
                last_activity = std::time::Instant::now();
                match chunk_result {
                    Some(result) => {
                        if let Ok(chunk) = &result
                            && let Some(running) = tool_running_after(chunk)
                        {
                            tool_running = running;
                        }
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
        let message = stalled_stream_message(STALL_TIMEOUT);
        assert!(message.contains("stopped responding"));
        assert!(message.contains("(no output for 3 minutes)"));
        assert!(message.contains("send a message"));
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

    /// AGE-453: a stream that carries only reasoning deltas for longer than
    /// the stall timeout is a busy model, not a stalled one. Before the fix
    /// `map_item` dropped those deltas, so this loop saw nothing and ended
    /// the turn as `Stalled` while the provider streamed the whole time.
    #[tokio::test]
    async fn reasoning_only_stream_is_not_a_stall() {
        let stall = StallPolicy {
            tick: std::time::Duration::from_millis(10),
            timeout: std::time::Duration::from_millis(60),
            tool_timeout: std::time::Duration::from_millis(60),
        };
        // 20 deltas, 10 ms apart: 200 ms of nothing but reasoning, more
        // than three timeouts long, then the answer.
        let chunks = futures::stream::iter(0..20)
            .then(|_| async {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                Ok(StreamChunk::Reasoning("hmm ".into()))
            })
            .chain(futures::stream::iter(vec![
                Ok(StreamChunk::Text("42".into())),
                Ok(StreamChunk::Done),
            ]));
        let mut stream: ResponseStream = Box::pin(chunks);
        let (_, mut progress_rx) = mpsc::unbounded_channel();
        let cancel_flag = Arc::new(AtomicBool::new(false));

        let mut handler = TestHandler::new();
        run_stream_loop_with(
            &mut stream,
            &mut progress_rx,
            &cancel_flag,
            &mut handler,
            stall,
        )
        .await
        .unwrap();

        assert!(
            !handler
                .chunks
                .iter()
                .any(|c| matches!(c, StreamChunk::Error(e) if e.kind == StreamErrorKind::Stalled)),
            "a reasoning-only stream was ended as stalled: {:?}",
            handler.chunks
        );
        assert!(matches!(handler.chunks.last(), Some(StreamChunk::Done)));
        assert_eq!(
            handler
                .chunks
                .iter()
                .filter(|c| matches!(c, StreamChunk::Reasoning(_)))
                .count(),
            20
        );
    }

    /// The same policy still ends a genuinely silent stream, so the test
    /// above is not passing because the watchdog stopped firing.
    #[tokio::test]
    async fn silent_stream_is_still_a_stall() {
        let stall = StallPolicy {
            tick: std::time::Duration::from_millis(10),
            timeout: std::time::Duration::from_millis(60),
            tool_timeout: std::time::Duration::from_millis(60),
        };
        let mut stream: ResponseStream = Box::pin(futures::stream::pending());
        let (_, mut progress_rx) = mpsc::unbounded_channel();
        let cancel_flag = Arc::new(AtomicBool::new(false));

        let mut handler = TestHandler::new();
        run_stream_loop_with(
            &mut stream,
            &mut progress_rx,
            &cancel_flag,
            &mut handler,
            stall,
        )
        .await
        .unwrap();

        assert!(matches!(
            handler.chunks.last(),
            Some(StreamChunk::Error(e)) if e.kind == StreamErrorKind::Stalled
        ));
    }

    /// A tool that runs longer than the stall timeout (a long
    /// `shell_execute` with its own `timeout_seconds`) is a busy turn, not
    /// a stalled one: between `ToolCallStarted` and the result the watchdog
    /// waits for `tool_timeout` instead.
    #[tokio::test]
    async fn a_running_tool_gets_the_tool_timeout() {
        let stall = StallPolicy {
            tick: std::time::Duration::from_millis(10),
            timeout: std::time::Duration::from_millis(60),
            tool_timeout: std::time::Duration::from_secs(10),
        };
        let chunks = futures::stream::iter(vec![
            Ok(StreamChunk::ToolCallStarted {
                id: "call_1".into(),
                name: "shell_execute".into(),
            }),
            Ok(StreamChunk::ToolCallInput {
                id: "call_1".into(),
                arguments: "{}".into(),
            }),
        ])
        .chain(futures::stream::once(async {
            // Four stall timeouts of silence while the tool runs.
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            Ok(StreamChunk::ToolCallResult {
                id: "call_1".into(),
                result: "ok".into(),
            })
        }))
        .chain(futures::stream::iter(vec![
            Ok(StreamChunk::Text("done".into())),
            Ok(StreamChunk::Done),
        ]));
        let mut stream: ResponseStream = Box::pin(chunks);
        let (_, mut progress_rx) = mpsc::unbounded_channel();
        let cancel_flag = Arc::new(AtomicBool::new(false));

        let mut handler = TestHandler::new();
        run_stream_loop_with(
            &mut stream,
            &mut progress_rx,
            &cancel_flag,
            &mut handler,
            stall,
        )
        .await
        .unwrap();

        assert!(
            matches!(handler.chunks.last(), Some(StreamChunk::Done)),
            "a long-running tool was ended as stalled: {:?}",
            handler.chunks
        );
    }

    /// Once the tool has answered, silence is measured against the normal
    /// timeout again.
    #[tokio::test]
    async fn silence_after_a_tool_result_is_still_a_stall() {
        let stall = StallPolicy {
            tick: std::time::Duration::from_millis(10),
            timeout: std::time::Duration::from_millis(60),
            tool_timeout: std::time::Duration::from_secs(3600),
        };
        let chunks = futures::stream::iter(vec![
            Ok(StreamChunk::ToolCallStarted {
                id: "call_1".into(),
                name: "shell_execute".into(),
            }),
            Ok(StreamChunk::ToolCallResult {
                id: "call_1".into(),
                result: "ok".into(),
            }),
        ])
        .chain(futures::stream::pending());
        let mut stream: ResponseStream = Box::pin(chunks);
        let (_, mut progress_rx) = mpsc::unbounded_channel();
        let cancel_flag = Arc::new(AtomicBool::new(false));

        let mut handler = TestHandler::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            run_stream_loop_with(
                &mut stream,
                &mut progress_rx,
                &cancel_flag,
                &mut handler,
                stall,
            ),
        )
        .await
        .expect("the watchdog fell back to the tool timeout after the result")
        .unwrap();

        assert!(matches!(
            handler.chunks.last(),
            Some(StreamChunk::Error(e)) if e.kind == StreamErrorKind::Stalled
        ));
    }

    /// The message names the timeout that fired: a stall inside the
    /// extended tool-call window used to say "3 minutes" after thirteen.
    #[test]
    fn stalled_stream_message_names_the_tool_timeout() {
        let message = stalled_stream_message(TOOL_STALL_TIMEOUT);
        assert!(message.contains("(no output for 13 minutes)"), "{message}");
        assert!(stalled_stream_message(std::time::Duration::from_secs(90)).contains("90 seconds"));
        assert!(stalled_stream_message(std::time::Duration::from_secs(60)).contains("1 minute)"));
    }

    /// A tool that never answers stalls on the tool timeout, and the error
    /// says so rather than quoting the plain one.
    #[tokio::test]
    async fn a_stall_during_a_tool_call_reports_the_tool_timeout() {
        let stall = StallPolicy {
            tick: std::time::Duration::from_millis(10),
            timeout: std::time::Duration::from_millis(60),
            tool_timeout: std::time::Duration::from_secs(1),
        };
        let chunks = futures::stream::iter(vec![Ok(StreamChunk::ToolCallStarted {
            id: "call_1".into(),
            name: "shell_execute".into(),
        })])
        .chain(futures::stream::pending());
        let mut stream: ResponseStream = Box::pin(chunks);
        let (_, mut progress_rx) = mpsc::unbounded_channel();
        let cancel_flag = Arc::new(AtomicBool::new(false));

        let mut handler = TestHandler::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            run_stream_loop_with(
                &mut stream,
                &mut progress_rx,
                &cancel_flag,
                &mut handler,
                stall,
            ),
        )
        .await
        .expect("the watchdog fires on the tool timeout")
        .unwrap();

        match handler.chunks.last() {
            Some(StreamChunk::Error(e)) => {
                assert_eq!(e.kind, StreamErrorKind::Stalled);
                assert_eq!(e.message, stalled_stream_message(stall.tool_timeout));
                assert!(e.message.contains("1 second"), "{}", e.message);
            }
            other => panic!("expected a stall error, got {other:?}"),
        }
    }

    #[test]
    fn the_tool_timeout_outlasts_the_longest_shell_call() {
        assert!(
            TOOL_STALL_TIMEOUT
                > std::time::Duration::from_secs(MAX_SHELL_CALL_TIMEOUT_SECONDS as u64),
            "the watchdog must not end a turn before shell_execute's own timeout fires"
        );
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
            StreamChunk::Reasoning(_) => "Reasoning",
            StreamChunk::ToolCallDelta => "ToolCallDelta",
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
                usage: None,
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

    const ALL_KINDS: [StreamErrorKind; 10] = [
        StreamErrorKind::Auth,
        StreamErrorKind::RateLimited,
        StreamErrorKind::ProviderStatus(503),
        StreamErrorKind::Transport,
        StreamErrorKind::MalformedToolCall,
        StreamErrorKind::Stalled,
        StreamErrorKind::Cancelled,
        StreamErrorKind::UnknownToolCall,
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
        for kind in [
            StreamErrorKind::MalformedToolCall,
            StreamErrorKind::UnknownToolCall,
        ] {
            for surface in [StreamSurface::Desktop, StreamSurface::InteractiveTui] {
                assert_eq!(decide_recovery(kind, surface, 0), RecoveryAction::Nudge);
                assert_eq!(decide_recovery(kind, surface, 1), RecoveryAction::Stop);
            }
        }
    }

    #[test]
    fn malformed_tool_call_uses_the_smaller_headless_budget() {
        let last_retry = HEADLESS_MALFORMED_JSON_RETRY_ATTEMPTS - 1;
        for kind in [
            StreamErrorKind::MalformedToolCall,
            StreamErrorKind::UnknownToolCall,
        ] {
            assert_eq!(
                decide_recovery(kind, StreamSurface::Headless, last_retry),
                RecoveryAction::Nudge
            );
            assert_eq!(
                decide_recovery(
                    kind,
                    StreamSurface::Headless,
                    HEADLESS_MALFORMED_JSON_RETRY_ATTEMPTS
                ),
                RecoveryAction::Stop
            );
        }
    }

    /// A stall resumes the turn only where nobody is there to type
    /// "continue", and only a bounded number of times.
    #[test]
    fn stall_resumes_only_on_headless_and_only_within_its_budget() {
        for surface in [StreamSurface::Desktop, StreamSurface::InteractiveTui] {
            assert_eq!(
                decide_recovery(StreamErrorKind::Stalled, surface, 0),
                RecoveryAction::Stop
            );
        }
        for attempt in 0..HEADLESS_STALL_RESUME_ATTEMPTS {
            assert_eq!(
                decide_recovery(StreamErrorKind::Stalled, StreamSurface::Headless, attempt),
                RecoveryAction::Retry {
                    after: std::time::Duration::ZERO
                }
            );
        }
        assert_eq!(
            decide_recovery(
                StreamErrorKind::Stalled,
                StreamSurface::Headless,
                HEADLESS_STALL_RESUME_ATTEMPTS
            ),
            RecoveryAction::Stop
        );
    }

    /// AGE-497: an unknown-tool call used to fall into `Other` (always
    /// `Stop`), ending the run outright on a one-token tool-name mistake.
    #[test]
    fn unknown_tool_call_is_not_lumped_into_other() {
        assert_ne!(
            decide_recovery(StreamErrorKind::UnknownToolCall, StreamSurface::Headless, 0),
            decide_recovery(StreamErrorKind::Other, StreamSurface::Headless, 0)
        );
    }

    #[test]
    fn cancelled_empty_and_other_always_stop() {
        for kind in [
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
