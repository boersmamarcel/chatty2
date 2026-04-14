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

    loop {
        if cancel_flag.load(Ordering::Relaxed) {
            handler.on_cancelled();
            break;
        }

        tokio::select! {
            biased;

            Some(progress) = progress_rx.recv() => {
                handler.on_progress(progress);
            }

            chunk_result = stream.next() => {
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
