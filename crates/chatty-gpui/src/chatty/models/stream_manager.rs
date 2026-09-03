use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gpui::{EventEmitter, Task};
use tracing::{debug, warn};

/// Minimum interval between batched TextChunk events (~200fps).
/// At 5ms each flush triggers one re-render cycle — fast enough for
/// responsive sustained streaming while avoiding per-character layout
/// thrashing. The *first* text chunk in a stream is always emitted
/// immediately (see `has_emitted_first_chunk`) so time-to-first-token
/// is not delayed by this interval.
const FLUSH_INTERVAL: Duration = Duration::from_millis(5);

use crate::chatty::services::StreamChunk;
use chatty_core::tools::PendingArtifacts;

/// Status of a stream lifecycle
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub enum StreamStatus {
    Active,
    Completed,
    Cancelled,
    Error(String),
}

/// Per-conversation stream state.
///
/// The StreamManager does NOT accumulate response text — that is the sole
/// responsibility of `ConversationsStore.streaming_message`. StreamManager
/// only tracks lifecycle (status, cancellation, token usage, trace).
pub struct StreamState {
    /// Identifies this registration among all streams for the conversation.
    ///
    /// `StreamEnded` is delivered through `cx.emit`, i.e. on the next effect
    /// flush — but a protocol follow-up registers the next stream synchronously
    /// right after `finalize_stream` returns. Without an epoch, turn N's end
    /// event lands on turn N+1 and tears it down (AGE-151).
    epoch: u64,
    pub status: StreamStatus,
    pub token_usage: Option<(u32, u32)>,
    pub trace_json: Option<serde_json::Value>,
    task: Option<Task<anyhow::Result<()>>>,
    cancel_flag: Arc<AtomicBool>,
    /// Shared reference to artifacts queued by AddAttachmentTool during this stream.
    /// Drained on finalization to include in StreamEnded event.
    pending_artifacts: Option<PendingArtifacts>,
    /// When `true`, the first text chunk has already been emitted immediately.
    /// Subsequent chunks are batched with `FLUSH_INTERVAL`.
    has_emitted_first_chunk: bool,
    /// Text accumulated since the last TextChunk event emission (batching buffer).
    pending_text: String,
    /// When the last TextChunk event was emitted (used for flush interval check).
    last_flush: Instant,
    /// Number of LLM API turns in this exchange. Starts at 1 (the initial request),
    /// incremented for each tool call result (which triggers another API call).
    /// Used to normalize rig-core's accumulated token usage back to per-turn values.
    api_turn_count: u32,
}

/// Events emitted by StreamManager for decoupled UI updates.
/// Each variant is tagged with `conversation_id` so subscribers can filter.
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub enum StreamManagerEvent {
    StreamStarted {
        conversation_id: String,
    },
    TextChunk {
        conversation_id: String,
        text: String,
    },
    ToolCallStarted {
        conversation_id: String,
        id: String,
        name: String,
    },
    ToolCallInput {
        conversation_id: String,
        id: String,
        arguments: String,
    },
    ToolCallResult {
        conversation_id: String,
        id: String,
        result: String,
    },
    ToolCallError {
        conversation_id: String,
        id: String,
        error: String,
    },
    ApprovalRequested {
        conversation_id: String,
        id: String,
        command: String,
        is_sandboxed: bool,
    },
    ApprovalResolved {
        conversation_id: String,
        id: String,
        approved: bool,
    },
    TokenUsage {
        conversation_id: String,
        input_tokens: u32,
        output_tokens: u32,
    },
    StreamEnded {
        conversation_id: String,
        /// Epoch of the stream that ended. Subscribers must ignore an event
        /// whose epoch is not the conversation's current one (AGE-151).
        epoch: u64,
        status: StreamStatus,
        token_usage: Option<(u32, u32)>,
        trace_json: Option<serde_json::Value>,
        /// Artifact paths queued by AddAttachmentTool during this stream.
        /// Non-empty only when status is Completed.
        pending_artifacts: Option<Vec<PathBuf>>,
        /// Number of LLM API turns in this exchange (1 = no tool calls).
        /// Used to normalize rig-core's accumulated token usage.
        api_turn_count: u32,
    },
}

/// Centralized stream lifecycle manager.
///
/// Owns stream lifecycle state (status, cancellation, token usage, trace) in a
/// `HashMap<String, StreamState>`. Does NOT accumulate response text — that is
/// the sole responsibility of `ConversationsStore.streaming_message` to avoid
/// dual-write divergence.
///
/// Emits `StreamManagerEvent` for decoupled UI updates.
/// Uses cancellation tokens (`Arc<AtomicBool>`) for graceful shutdown.
pub struct StreamManager {
    streams: HashMap<String, StreamState>,
    pending_resolved_ids: HashMap<String, Arc<Mutex<Option<String>>>>,
    /// Monotonic counter handing every registered stream a distinct epoch.
    next_epoch: u64,
    /// Epoch of the most recent registration per conversation, kept after the
    /// stream is removed so a late `StreamEnded` can still be recognised as
    /// stale.
    current_epoch: HashMap<String, u64>,
}

impl EventEmitter<StreamManagerEvent> for StreamManager {}

impl StreamManager {
    pub fn new() -> Self {
        Self {
            streams: HashMap::new(),
            pending_resolved_ids: HashMap::new(),
            next_epoch: 1,
            current_epoch: HashMap::new(),
        }
    }

    /// Claim the next epoch for `conv_id` and record it as current.
    fn claim_epoch(&mut self, conv_id: &str) -> u64 {
        let epoch = self.next_epoch;
        self.next_epoch = self.next_epoch.wrapping_add(1);
        self.current_epoch.insert(conv_id.to_string(), epoch);
        epoch
    }

    /// Whether `epoch` is the conversation's latest registration.
    ///
    /// An unknown conversation answers `true` so events for streams that
    /// predate epoch tracking (or for `__pending__` before promotion) are not
    /// silently dropped.
    pub fn is_current_epoch(&self, conv_id: &str, epoch: u64) -> bool {
        match self.current_epoch.get(conv_id) {
            Some(current) => *current == epoch,
            None => true,
        }
    }

    /// Register a stream for a known conversation.
    /// If a stream already exists for this conversation, it is cancelled first
    /// with proper cleanup (emits StreamEnded with Cancelled status).
    pub fn register_stream(
        &mut self,
        conv_id: String,
        task: Task<anyhow::Result<()>>,
        cancel_flag: Arc<AtomicBool>,
        pending_artifacts: Option<PendingArtifacts>,
        cx: &mut gpui::Context<Self>,
    ) {
        // Cancel existing stream if any — emit StreamEnded so subscribers
        // (app_controller) can transition Running tool calls to Cancelled.
        if let Some(mut existing) = self.streams.remove(&conv_id) {
            // Flush any buffered text
            Self::flush_pending_text_for(&mut existing, &conv_id, cx);

            existing.cancel_flag.store(true, Ordering::Relaxed);

            let token_usage = existing.token_usage;
            let trace_json = existing.trace_json.clone();
            let turn_count = existing.api_turn_count;
            let ended_epoch = existing.epoch;

            debug!(conv_id = %conv_id, "Cancelled existing stream before registering new one");

            // Drop the task before emitting so GPUI aborts it promptly
            drop(existing.task.take());

            cx.emit(StreamManagerEvent::StreamEnded {
                conversation_id: conv_id.clone(),
                epoch: ended_epoch,
                status: StreamStatus::Cancelled,
                token_usage,
                trace_json,
                pending_artifacts: None,
                api_turn_count: turn_count,
            });
        }

        let epoch = self.claim_epoch(&conv_id);
        self.streams.insert(
            conv_id.clone(),
            StreamState {
                epoch,
                status: StreamStatus::Active,
                token_usage: None,
                trace_json: None,
                task: Some(task),
                cancel_flag,
                pending_artifacts,
                has_emitted_first_chunk: false,
                pending_text: String::with_capacity(256),
                last_flush: Instant::now(),
                api_turn_count: 1,
            },
        );

        cx.emit(StreamManagerEvent::StreamStarted {
            conversation_id: conv_id,
        });
    }

    /// Register a stream that doesn't have a conversation ID yet.
    /// The stream is stored under `"__pending__"` and can be promoted later.
    pub fn register_pending_stream(
        &mut self,
        task: Task<anyhow::Result<()>>,
        resolved_id: Arc<Mutex<Option<String>>>,
        cancel_flag: Arc<AtomicBool>,
        pending_artifacts: Option<PendingArtifacts>,
        cx: &mut gpui::Context<Self>,
    ) {
        // Cancel any existing pending stream — emit StreamEnded so subscribers
        // (app_controller) can transition Running tool calls to Cancelled.
        if let Some(mut existing) = self.streams.remove("__pending__") {
            Self::flush_pending_text_for(&mut existing, "__pending__", cx);

            existing.cancel_flag.store(true, Ordering::Relaxed);

            let token_usage = existing.token_usage;
            let trace_json = existing.trace_json.clone();
            let turn_count = existing.api_turn_count;

            debug!("Cancelled existing pending stream");

            let ended_epoch = existing.epoch;
            drop(existing.task.take());

            cx.emit(StreamManagerEvent::StreamEnded {
                conversation_id: "__pending__".to_string(),
                epoch: ended_epoch,
                status: StreamStatus::Cancelled,
                token_usage,
                trace_json,
                pending_artifacts: None,
                api_turn_count: turn_count,
            });
        }

        let epoch = self.claim_epoch("__pending__");
        self.streams.insert(
            "__pending__".to_string(),
            StreamState {
                epoch,
                status: StreamStatus::Active,
                token_usage: None,
                trace_json: None,
                task: Some(task),
                cancel_flag,
                pending_artifacts,
                has_emitted_first_chunk: false,
                pending_text: String::with_capacity(256),
                last_flush: Instant::now(),
                api_turn_count: 1,
            },
        );

        self.pending_resolved_ids
            .insert("__pending__".to_string(), resolved_id);

        cx.emit(StreamManagerEvent::StreamStarted {
            conversation_id: "__pending__".to_string(),
        });
    }

    /// Promote a pending stream to a real conversation ID.
    /// Called once the conversation has been created.
    pub fn promote_pending(&mut self, conv_id: &str) {
        if let Some(state) = self.streams.remove("__pending__") {
            debug!(conv_id = %conv_id, "Promoting pending stream to conversation");
            // The epoch moves with the stream, so a StreamEnded emitted under
            // either key still matches the conversation's current epoch.
            self.current_epoch.insert(conv_id.to_string(), state.epoch);
            self.streams.insert(conv_id.to_string(), state);
        }
        self.pending_resolved_ids.remove("__pending__");
        self.current_epoch.remove("__pending__");
    }

    /// Set the pending artifacts handle on a promoted stream.
    /// Called after `promote_pending()` to wire up the conversation's artifact storage
    /// so that `finalize_stream()` can drain artifacts queued by `AddAttachmentTool`.
    pub fn set_pending_artifacts(&mut self, conv_id: &str, artifacts: PendingArtifacts) {
        if let Some(state) = self.streams.get_mut(conv_id) {
            state.pending_artifacts = Some(artifacts);
        }
    }

    /// Emit any accumulated pending text for a conversation as a `TextChunk` event.
    /// No-op if there is no pending text.
    fn flush_pending_text_for(
        state: &mut StreamState,
        conv_id: &str,
        cx: &mut gpui::Context<StreamManager>,
    ) {
        if !state.pending_text.is_empty() {
            let batch = std::mem::take(&mut state.pending_text);
            state.last_flush = Instant::now();
            cx.emit(StreamManagerEvent::TextChunk {
                conversation_id: conv_id.to_string(),
                text: batch,
            });
        }
    }

    fn flush_pending_text(&mut self, conv_id: &str, cx: &mut gpui::Context<Self>) {
        if let Some(state) = self.streams.get_mut(conv_id)
            && !state.pending_text.is_empty()
        {
            Self::flush_pending_text_for(state, conv_id, cx);
        }
    }

    /// Process a stream chunk: update internal state and emit the corresponding event.
    ///
    /// Text chunks use a hybrid strategy: the *first* chunk is emitted immediately
    /// (zero latency), then subsequent chunks are batched and emitted only when
    /// `FLUSH_INTERVAL` (5ms, ~200fps) has elapsed. All other chunk types are forwarded
    /// immediately without delay.
    pub fn handle_chunk(
        &mut self,
        conv_id: &str,
        chunk: StreamChunk,
        cx: &mut gpui::Context<Self>,
    ) {
        match chunk {
            StreamChunk::Text(text) => {
                if let Some(state) = self.streams.get_mut(conv_id) {
                    state.pending_text.push_str(&text);
                    if !state.has_emitted_first_chunk {
                        // First chunk → emit immediately for minimal time-to-first-token
                        state.has_emitted_first_chunk = true;
                        let batch = std::mem::take(&mut state.pending_text);
                        state.last_flush = Instant::now();
                        cx.emit(StreamManagerEvent::TextChunk {
                            conversation_id: conv_id.to_string(),
                            text: batch,
                        });
                    } else if state.last_flush.elapsed() >= FLUSH_INTERVAL {
                        // Subsequent chunks → respect the flush interval to avoid thrashing
                        let batch = std::mem::take(&mut state.pending_text);
                        state.last_flush = Instant::now();
                        cx.emit(StreamManagerEvent::TextChunk {
                            conversation_id: conv_id.to_string(),
                            text: batch,
                        });
                    }
                }
            }
            StreamChunk::ToolCallStarted { id, name } => {
                cx.emit(StreamManagerEvent::ToolCallStarted {
                    conversation_id: conv_id.to_string(),
                    id,
                    name,
                });
            }
            StreamChunk::ToolCallInput { id, arguments } => {
                cx.emit(StreamManagerEvent::ToolCallInput {
                    conversation_id: conv_id.to_string(),
                    id,
                    arguments,
                });
            }
            StreamChunk::ToolCallResult { id, result } => {
                // Each tool result triggers another API call, so increment the turn count.
                // This is used to normalize rig-core's accumulated token usage.
                if let Some(state) = self.streams.get_mut(conv_id) {
                    state.api_turn_count += 1;
                }
                cx.emit(StreamManagerEvent::ToolCallResult {
                    conversation_id: conv_id.to_string(),
                    id,
                    result,
                });
            }
            StreamChunk::ToolCallError { id, error } => {
                // A tool error still triggers an API round-trip (the error is
                // sent back to the model as a tool result), so increment the
                // turn count here just as we do for a successful ToolCallResult.
                if let Some(state) = self.streams.get_mut(conv_id) {
                    state.api_turn_count += 1;
                }
                cx.emit(StreamManagerEvent::ToolCallError {
                    conversation_id: conv_id.to_string(),
                    id,
                    error,
                });
            }
            StreamChunk::ApprovalRequested {
                id,
                command,
                is_sandboxed,
            } => {
                cx.emit(StreamManagerEvent::ApprovalRequested {
                    conversation_id: conv_id.to_string(),
                    id,
                    command,
                    is_sandboxed,
                });
            }
            StreamChunk::ApprovalResolved { id, approved } => {
                cx.emit(StreamManagerEvent::ApprovalResolved {
                    conversation_id: conv_id.to_string(),
                    id,
                    approved,
                });
            }
            StreamChunk::TokenUsage {
                input_tokens,
                output_tokens,
            } => {
                if let Some(state) = self.streams.get_mut(conv_id) {
                    state.token_usage = Some((input_tokens, output_tokens));
                }
                cx.emit(StreamManagerEvent::TokenUsage {
                    conversation_id: conv_id.to_string(),
                    input_tokens,
                    output_tokens,
                });
            }
            StreamChunk::Done => {
                // Don't finalize yet — caller should call finalize_stream()
            }
            StreamChunk::Error(error) => {
                // Flush any buffered text before emitting StreamEnded
                self.flush_pending_text(conv_id, cx);
                let (token_usage, trace_json, turn_count, epoch) =
                    if let Some(state) = self.streams.get_mut(conv_id) {
                        state.status = StreamStatus::Error(error.clone());
                        (
                            state.token_usage,
                            state.trace_json.clone(),
                            state.api_turn_count,
                            state.epoch,
                        )
                    } else {
                        (None, None, 1, 0)
                    };
                cx.emit(StreamManagerEvent::StreamEnded {
                    conversation_id: conv_id.to_string(),
                    epoch,
                    status: StreamStatus::Error(error),
                    token_usage,
                    trace_json,
                    pending_artifacts: None,
                    api_turn_count: turn_count,
                });
                self.streams.remove(conv_id);
            }
        }
    }

    /// Mark a stream as completed and emit StreamEnded.
    /// Called when the stream loop finishes normally.
    /// Flushes any pending batched text, then drains any pending artifacts queued by AddAttachmentTool.
    pub fn finalize_stream(&mut self, conv_id: &str, cx: &mut gpui::Context<Self>) {
        // Flush any remaining buffered text before emitting StreamEnded
        self.flush_pending_text(conv_id, cx);

        let (token_usage, trace_json, artifacts, turn_count, epoch) =
            if let Some(state) = self.streams.get(conv_id) {
                let drained = state
                    .pending_artifacts
                    .as_ref()
                    .and_then(|pa| pa.lock().ok())
                    .map(|mut v| v.drain(..).collect::<Vec<_>>())
                    .filter(|v| !v.is_empty());
                (
                    state.token_usage,
                    state.trace_json.clone(),
                    drained,
                    state.api_turn_count,
                    state.epoch,
                )
            } else {
                warn!(conv_id = %conv_id, "finalize_stream called but no stream found");
                return;
            };

        cx.emit(StreamManagerEvent::StreamEnded {
            conversation_id: conv_id.to_string(),
            epoch,
            status: StreamStatus::Completed,
            token_usage,
            trace_json,
            pending_artifacts: artifacts,
            api_turn_count: turn_count,
        });

        self.streams.remove(conv_id);
    }

    /// Gracefully stop a stream using its cancellation token.
    pub fn stop_stream(&mut self, conv_id: &str, cx: &mut gpui::Context<Self>) {
        // Try direct key first
        let key = if self.streams.contains_key(conv_id) {
            Some(conv_id.to_string())
        } else if self.streams.contains_key("__pending__") {
            // Check if pending stream resolved to this conversation
            let is_match = self
                .pending_resolved_ids
                .get("__pending__")
                .and_then(|resolved| resolved.lock().ok())
                .map(|resolved| resolved.as_ref() == Some(&conv_id.to_string()))
                .unwrap_or(false);
            if is_match {
                Some("__pending__".to_string())
            } else {
                // Pending stream belongs to a different conversation, don't cancel it
                None
            }
        } else {
            None
        };

        let Some(key) = key else { return };

        if let Some(mut state) = self.streams.remove(&key) {
            // Flush any buffered text before the cancellation event
            if !state.pending_text.is_empty() {
                let batch = std::mem::take(&mut state.pending_text);
                cx.emit(StreamManagerEvent::TextChunk {
                    conversation_id: conv_id.to_string(),
                    text: batch,
                });
            }

            // Set cancellation flag for graceful shutdown
            state.cancel_flag.store(true, Ordering::Relaxed);
            state.status = StreamStatus::Cancelled;

            let token_usage = state.token_usage;
            let trace_json = state.trace_json.clone();
            let turn_count = state.api_turn_count;
            let epoch = state.epoch;

            debug!(conv_id = %conv_id, "Stream stopped gracefully");

            // Drop the task (backstop — the cancel flag should cause clean exit)
            drop(state.task.take());

            cx.emit(StreamManagerEvent::StreamEnded {
                conversation_id: conv_id.to_string(),
                epoch,
                status: StreamStatus::Cancelled,
                token_usage,
                trace_json,
                pending_artifacts: None,
                api_turn_count: turn_count,
            });

            // Clean up pending resolved IDs if we used the pending key
            if key == "__pending__" {
                self.pending_resolved_ids.remove("__pending__");
            }
        }
    }

    /// Cancel any pending stream (used when creating a new conversation).
    pub fn cancel_pending(&mut self, cx: &mut gpui::Context<Self>) {
        if let Some(mut state) = self.streams.remove("__pending__") {
            // Flush any buffered text before the cancellation event
            if !state.pending_text.is_empty() {
                let batch = std::mem::take(&mut state.pending_text);
                cx.emit(StreamManagerEvent::TextChunk {
                    conversation_id: "__pending__".to_string(),
                    text: batch,
                });
            }

            state.cancel_flag.store(true, Ordering::Relaxed);
            debug!("Cancelled pending stream");
            cx.emit(StreamManagerEvent::StreamEnded {
                conversation_id: "__pending__".to_string(),
                epoch: state.epoch,
                status: StreamStatus::Cancelled,
                token_usage: state.token_usage,
                trace_json: state.trace_json,
                pending_artifacts: None,
                api_turn_count: state.api_turn_count,
            });
        }
        self.pending_resolved_ids.remove("__pending__");
        self.current_epoch.remove("__pending__");
    }

    /// Check if a conversation has an active stream.
    /// Also checks pending streams that may have resolved to this conversation.
    pub fn is_streaming(&self, conv_id: &str) -> bool {
        if self.streams.contains_key(conv_id) {
            return true;
        }

        // Check if a pending stream has resolved to this conversation ID
        self.pending_resolved_ids
            .get("__pending__")
            .and_then(|resolved| resolved.lock().ok())
            .map(|resolved| resolved.as_ref() == Some(&conv_id.to_string()))
            .unwrap_or(false)
    }

    /// Check if any stream is active.
    #[allow(dead_code)]
    pub fn has_active_streams(&self) -> bool {
        !self.streams.is_empty()
    }

    /// Set trace JSON on an active stream (called before finalization).
    pub fn set_trace(&mut self, conv_id: &str, trace: Option<serde_json::Value>) {
        if let Some(state) = self.streams.get_mut(conv_id) {
            state.trace_json = trace;
        }
    }

    /// Stop all active streams (app shutdown).
    pub fn stop_all(&mut self, cx: &mut gpui::Context<Self>) {
        let keys: Vec<String> = self.streams.keys().cloned().collect();
        for key in keys {
            if let Some(mut state) = self.streams.remove(&key) {
                // Flush any buffered text before the cancellation event
                if !state.pending_text.is_empty() {
                    let batch = std::mem::take(&mut state.pending_text);
                    cx.emit(StreamManagerEvent::TextChunk {
                        conversation_id: key.clone(),
                        text: batch,
                    });
                }

                state.cancel_flag.store(true, Ordering::Relaxed);
                cx.emit(StreamManagerEvent::StreamEnded {
                    conversation_id: key,
                    epoch: state.epoch,
                    status: StreamStatus::Cancelled,
                    token_usage: state.token_usage,
                    trace_json: state.trace_json,
                    pending_artifacts: None,
                    api_turn_count: state.api_turn_count,
                });
            }
        }
        self.pending_resolved_ids.clear();
        self.current_epoch.clear();
    }
}

/// Global accessor for the StreamManager entity.
/// Stores a strong `Entity` reference to prevent the StreamManager from being
/// garbage collected when the initialization closure's local variables go out of scope.
pub type GlobalStreamManager = crate::global_entity::GlobalStrongEntity<StreamManager>;

#[cfg(test)]
mod tests {
    use super::*;

    // -------------------------------------------------------------------
    // Stream epoch (AGE-151)
    //
    // `StreamEnded` is delivered on the next effect flush, but a protocol
    // follow-up registers the next stream synchronously right after
    // `finalize_stream` returns. The epoch is what lets the subscriber tell
    // turn N's late end event from turn N+1's.
    // -------------------------------------------------------------------

    fn active_state(epoch: u64) -> StreamState {
        StreamState {
            epoch,
            status: StreamStatus::Active,
            token_usage: None,
            trace_json: None,
            task: None,
            cancel_flag: Arc::new(AtomicBool::new(false)),
            pending_artifacts: None,
            has_emitted_first_chunk: false,
            pending_text: String::new(),
            last_flush: Instant::now(),
            api_turn_count: 1,
        }
    }

    #[test]
    fn each_registration_claims_a_distinct_epoch() {
        let mut mgr = StreamManager::new();
        let first = mgr.claim_epoch("conv-1");
        let second = mgr.claim_epoch("conv-1");
        assert_ne!(first, second);
    }

    /// The guard the follow-up race needs: once the next turn is registered,
    /// the previous turn's still-undelivered end event is stale.
    #[test]
    fn a_superseded_epoch_is_not_current() {
        let mut mgr = StreamManager::new();
        let first = mgr.claim_epoch("conv-1");
        assert!(mgr.is_current_epoch("conv-1", first));

        let second = mgr.claim_epoch("conv-1");
        assert!(
            !mgr.is_current_epoch("conv-1", first),
            "turn N's end event must not be applied to turn N+1"
        );
        assert!(mgr.is_current_epoch("conv-1", second));
    }

    /// The epoch outlives the stream entry, because the whole point is to
    /// recognise an event that arrives after the stream was removed.
    #[test]
    fn epoch_survives_the_stream_being_removed() {
        let mut mgr = StreamManager::new();
        let first = mgr.claim_epoch("conv-1");
        mgr.streams.insert("conv-1".to_string(), active_state(first));
        mgr.streams.remove("conv-1");
        assert!(mgr.is_current_epoch("conv-1", first));

        let second = mgr.claim_epoch("conv-1");
        assert!(!mgr.is_current_epoch("conv-1", first));
        assert!(mgr.is_current_epoch("conv-1", second));
    }

    /// Conversations do not interfere with each other.
    #[test]
    fn epochs_are_tracked_per_conversation() {
        let mut mgr = StreamManager::new();
        let a = mgr.claim_epoch("conv-a");
        let _b = mgr.claim_epoch("conv-b");
        assert!(mgr.is_current_epoch("conv-a", a));
    }

    /// A stream registered before epoch tracking (or an event for a
    /// conversation we know nothing about) must not be silently dropped.
    #[test]
    fn unknown_conversation_accepts_its_event() {
        let mgr = StreamManager::new();
        assert!(mgr.is_current_epoch("never-seen", 0));
    }

    /// Promotion moves the epoch with the stream, so an end event emitted
    /// under either key still matches.
    #[test]
    fn promotion_carries_the_epoch_to_the_real_conversation() {
        let mut mgr = StreamManager::new();
        let epoch = mgr.claim_epoch("__pending__");
        mgr.streams
            .insert("__pending__".to_string(), active_state(epoch));

        mgr.promote_pending("conv-9");
        assert!(mgr.is_current_epoch("conv-9", epoch));
    }

    #[test]
    fn test_new_stream_manager_is_empty() {
        let mgr = StreamManager::new();
        assert!(!mgr.has_active_streams());
        assert!(!mgr.is_streaming("test"));
    }

    #[test]
    fn test_is_streaming_with_pending_resolved() {
        let mut mgr = StreamManager::new();
        let resolved = Arc::new(Mutex::new(Some("conv-123".to_string())));
        mgr.pending_resolved_ids
            .insert("__pending__".to_string(), resolved);
        // Manually insert a pending stream state (without task/cancel_flag for test)
        mgr.streams.insert(
            "__pending__".to_string(),
            StreamState {
                epoch: 1,
                status: StreamStatus::Active,
                token_usage: None,
                trace_json: None,
                task: None,
                cancel_flag: Arc::new(AtomicBool::new(false)),
                pending_artifacts: None,
                has_emitted_first_chunk: false,
                pending_text: String::new(),
                last_flush: Instant::now(),
                api_turn_count: 1,
            },
        );
        assert!(mgr.is_streaming("conv-123"));
        assert!(!mgr.is_streaming("other"));
    }

    #[test]
    fn test_promote_pending() {
        let mut mgr = StreamManager::new();
        mgr.streams.insert(
            "__pending__".to_string(),
            StreamState {
                epoch: 1,
                status: StreamStatus::Active,
                token_usage: None,
                trace_json: None,
                task: None,
                cancel_flag: Arc::new(AtomicBool::new(false)),
                