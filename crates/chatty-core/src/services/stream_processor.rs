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
pub const STALLED_STREAM_MESSAGE: &str =
    "The model stopped responding (no output for 3 minutes). The turn was ended \
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
                    handler.on_chunk(Ok(StreamChunk::Error(
                        STALLED_STREAM_MESSAGE.to_string(),
                    )))?;
                    break;
                }
            }

            chunk_result = stream.next() => {
                last_activity = std::time::Instant::now();
                match chunk_result {
                    Some(result) => {
                        match handler.on_chunk(result)? {
                            ChunkAction::Continue => {}
                            ChunkAction::Break => break,
                        }
                    }
                    None => break,
                }
            }
        }
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
}
