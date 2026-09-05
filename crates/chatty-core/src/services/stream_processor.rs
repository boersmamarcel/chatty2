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

/// Trait for handling stream chunks and progress events.
///
/// Both the GPUI and TUI frontends implement this trait to receive stream
/// events through their respective UI update mechanisms (GPUI entity updates
/// vs. channel-based event dispatch).
///
/// `on_chunk` is async because the desktop refreshes an expired Azure token in
/// place when a 401 arrives mid-stream, and has to await it before deciding
/// whether the turn is over. No `Send` bound: the desktop's handler holds an
/// `AsyncApp`, which is deliberately not `Send`.
#[allow(async_fn_in_trait)]
pub trait StreamChunkHandler {
    /// Called once when the stream loop starts (before the first chunk).
    fn on_stream_started(&mut self);

    /// Called for each LLM stream chunk. Return [`ChunkAction::Break`] to stop.
    async fn on_chunk(&mut self, chunk: Result<StreamChunk>) -> Result<ChunkAction>;

    /// Called for each sub-agent progress event from `invoke_agent`.
    fn on_progress(&mut self, progress: InvokeAgentProgress);

    /// Called when the stream loop exits due to cancellation.
    fn on_cancelled(&mut self);

    /// Called after the stream loop finishes (whether normally or via error/cancel).
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
                    handler
                        .on_chunk(Ok(StreamChunk::Error(
                            STALLED_STREAM_MESSAGE.to_string(),
                        )))
                        .await?;
                    break;
                }
            }

            chunk_result = stream.next() => {
                last_activity = std::time::Instant::now();
                match chunk_result {
                    Some(result) => {
                        match handler.on_chunk(result).await? {
                            ChunkAction::Continue => {}
                            ChunkAction::Break => break,
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
    Ok(())
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

        async fn on_chunk(&mut self, chunk: Result<StreamChunk>) -> Result<ChunkAction> {
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
            StreamChunk::TokenUsage { .. } => "TokenUsage",
            StreamChunk::Done => "Done",
            StreamChunk::Error(_) => "Error",
        }
    }

    impl StreamChunkHandler for RecordingHandler {
        fn on_stream_started(&mut self) {
            self.calls.push("on_stream_started".to_string());
        }

        async fn on_chunk(&mut self, chunk: Result<StreamChunk>) -> Result<ChunkAction> {
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
            .await
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
}
