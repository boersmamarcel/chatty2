use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gpui::{App, BorrowAppContext as _, EventEmitter, Task};
use tracing::{debug, warn};

use chatty_core::models::ConversationsStore;
use chatty_core::session::SessionEvent;

/// Minimum interval between batched TextChunk events, and the period of the
/// background flush timer that backstops it (AGE-166).
///
/// 5ms (~200fps) exceeded every real display's refresh rate for no benefit,
/// while still round-tripping through `ConversationsStore::append_streaming_content`
/// and `StreamManager::handle_chunk` on every single raw LLM chunk. 20ms
/// (~50fps) sits in the ~16-33ms band a 30-60Hz display can actually paint,
/// and keeps the emitted TextChunk rate comfortably under the ~60/s budget.
/// The *first* text chunk in a stream is always emitted immediately (see
/// `has_emitted_first_chunk`) so time-to-first-token is not delayed by this
/// interval, and `ensure_flush_timer` flushes on this same cadence even when
/// no further chunk arrives, so a pause mid-interval still paints instead of
/// waiting for the next token.
pub(crate) const FLUSH_INTERVAL: Duration = Duration::from_millis(20);

/// Whether buffered text should flush now: the first chunk of a turn always
/// flushes immediately (protects time-to-first-token); after that, at most
/// once per `FLUSH_INTERVAL`. Shared by `StreamManager`'s own UI-facing
/// batching below and by `DesktopSink`'s upstream batching
/// (`message_ops_internals.rs`), so the two hot paths this issue coalesces
/// implement one flush policy, not two (AGE-166).
pub(crate) fn should_flush_text(is_first_chunk: bool, since_last_flush: Duration) -> bool {
    is_first_chunk || since_last_flush >= FLUSH_INTERVAL
}

/// The one buffer of coalesced streaming text for a turn on the desktop.
///
/// `DesktopSink` (`message_ops_internals.rs`) fills it — raw
/// `SessionEvent::Text` chunks arrive far faster than any display repaints,
/// and coalescing them here is what keeps one `update_global` into the
/// conversation and one `StreamManager` entity update per `FLUSH_INTERVAL`
/// instead of per token (AGE-166).
///
/// It is *shared* rather than owned by the sink, because the sink lives
/// inside the turn's future and `stop_stream`, `cancel_pending` and both
/// `register_*_stream` supersede paths drop that future synchronously: a
/// buffer only the sink could reach lost up to one `FLUSH_INTERVAL` of the
/// reply on every Stop, in the UI and in the persisted message (AGE-372).
/// `StreamState` therefore keeps a handle, and `StreamManager::flush_text`
/// drains this batch before its own `pending_text` on every one of those
/// paths and on each `ensure_flush_timer` tick — so the timer backstops the
/// layer that actually buffers.
pub(crate) struct TextBatch {
    /// The conversation whose session buffered text is applied to. Held
    /// here because `StreamManager` may key the same stream as
    /// `__pending__`.
    conv_id: String,
    pending: String,
    has_flushed_first: bool,
    last_flush: Instant,
}

/// A [`TextBatch`] as the sink and the stream's `StreamState` both hold it.
/// `Rc`, not `Arc`: the sink holds an `AsyncApp` and `StreamManager` is a
/// GPUI entity, so neither side ever leaves the main thread.
pub(crate) type SharedTextBatch = Rc<RefCell<TextBatch>>;

impl TextBatch {
    fn new(conv_id: String) -> Self {
        Self {
            conv_id,
            pending: String::new(),
            has_flushed_first: false,
            last_flush: Instant::now(),
        }
    }

    pub(crate) fn shared(conv_id: String) -> SharedTextBatch {
        Rc::new(RefCell::new(Self::new(conv_id)))
    }

    /// Buffer `text`, answering whether the batch should go out now: the
    /// turn's first chunk immediately (time-to-first-token is never delayed
    /// by batching), then at most once per `FLUSH_INTERVAL`
    /// (`should_flush_text`). Buffering only — the caller flushes with
    /// [`TextBatch::drain`].
    pub(crate) fn push(&mut self, text: &str) -> bool {
        self.pending.push_str(text);
        should_flush_text(!self.has_flushed_first, self.last_flush.elapsed())
    }

    /// Take everything buffered, regardless of the flush policy. `None`
    /// when nothing is pending, so flushing before a tool call never emits
    /// a spurious empty `TextChunk`.
    fn take(&mut self) -> Option<String> {
        if self.pending.is_empty() {
            return None;
        }
        self.has_flushed_first = true;
        self.last_flush = Instant::now();
        Some(std::mem::take(&mut self.pending))
    }

    /// Take everything buffered and apply it to the conversation's session,
    /// returning it so the caller can forward it onwards as one
    /// `SessionEvent::Text`.
    ///
    /// This is the single path buffered text takes into
    /// `Conversation.streaming_message` — which is what
    /// `finalize_stopped_stream` persists — so a drain driven by
    /// `StreamManager` and a flush driven by `DesktopSink` cannot disagree.
    pub(crate) fn drain(&mut self, cx: &mut App) -> Option<String> {
        let text = self.take()?;
        if !cx.has_global::<ConversationsStore>() {
            warn!(conv_id = %self.conv_id, "No ConversationsStore to apply buffered text to");
            return Some(text);
        }
        let conv_id = self.conv_id.clone();
        let event = SessionEvent::Text(text.clone());
        cx.update_global::<ConversationsStore, _>(|store, _cx| {
            store
                .get_session_mut(&conv_id)
                .and_then(|session| session.apply(&event));
        });
        Some(text)
    }
}

use crate::chatty::services::StreamChunk;
use chatty_core::models::token_usage::{ApiCallUsage, TokenUsage};
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
    /// Usage for the turn, built from the per-request records below once the
    /// provider's aggregate arrives. `None` until then.
    pub token_usage: Option<TokenUsage>,
    /// One record per completed provider request, in order. Cache hit rate is
    /// a per-request property, so these are the source of truth and the
    /// aggregate is derived from them.
    calls: Vec<ApiCallUsage>,
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
    /// Handle on the `DesktopSink` batch buffering this turn's text one
    /// layer upstream of `pending_text`, so every path here that drops the
    /// turn's task can drain it first (AGE-372). `None` for a stream with no
    /// desktop sink — the characterization harness drives
    /// `handle_session_event` directly.
    text_batch: Option<SharedTextBatch>,
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
    ClarificationRequested {
        conversation_id: String,
        id: String,
        questions: Vec<chatty_core::models::clarification_store::ClarifyingQuestion>,
    },
    TokenUsage {
        conversation_id: String,
        input_tokens: u32,
        output_tokens: u32,
        cache_read_tokens: u32,
        cache_write_tokens: u32,
    },
    StreamEnded {
        conversation_id: String,
        /// Epoch of the stream that ended. Subscribers must ignore an event
        /// whose epoch is not the conversation's current one (AGE-151).
        epoch: u64,
        status: StreamStatus,
        token_usage: Option<TokenUsage>,
        trace_json: Option<serde_json::Value>,
        /// Artifact paths queued by AddAttachmentTool during this stream.
        /// Non-empty only when status is Completed.
        pending_artifacts: Option<Vec<PathBuf>>,
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
    /// Background timer that flushes any buffered text every `FLUSH_INTERVAL`,
    /// so a pause mid-interval still paints instead of waiting for the next
    /// chunk to trigger the elapsed check (AGE-166). Spawned lazily on the
    /// first stream and stops itself once no streams remain, so an idle app
    /// doesn't keep waking the foreground executor at ~50Hz forever.
    flush_timer: Option<Task<()>>,
}

impl EventEmitter<StreamManagerEvent> for StreamManager {}

impl StreamManager {
    pub fn new() -> Self {
        Self {
            streams: HashMap::new(),
            pending_resolved_ids: HashMap::new(),
            next_epoch: 1,
            current_epoch: HashMap::new(),
            flush_timer: None,
        }
    }

    /// Start the periodic flush timer the first time a stream needs it. The
    /// timer stops itself and clears `flush_timer` once no streams remain;
    /// this call transparently respawns it on the next stream (see
    /// `flush_timer`'s docs).
    fn ensure_flush_timer(&mut self, cx: &mut gpui::Context<Self>) {
        if self.flush_timer.is_some() {
            return;
        }
        self.flush_timer = Some(cx.spawn(async move |entity, cx| {
            loop {
                cx.background_executor().timer(FLUSH_INTERVAL).await;
                let should_continue = entity
                    .update(cx, |sm, cx| {
                        let conv_ids: Vec<String> = sm.streams.keys().cloned().collect();
                        for conv_id in conv_ids {
                            sm.flush_text(&conv_id, &conv_id, cx);
                        }
                        if sm.streams.is_empty() {
                            // Nothing left to flush — stop ticking rather than
                            // spinning at FLUSH_INTERVAL for the rest of the
                            // process's life. `ensure_flush_timer` respawns
                            // this on the next `register_stream` /
                            // `register_pending_stream` (AGE-166 review).
                            sm.flush_timer = None;
                            false
                        } else {
                            true
                        }
                    })
                    .unwrap_or(false);
                if !should_continue {
                    break;
                }
            }
        }));
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
        self.ensure_flush_timer(cx);

        // Flush every buffered layer — including the superseded turn's own
        // `DesktopSink` batch — before the task below is dropped (AGE-372).
        self.flush_text(&conv_id, &conv_id, cx);

        // Cancel existing stream if any — emit StreamEnded so subscribers
        // (app_controller) can transition Running tool calls to Cancelled.
        if let Some(mut existing) = self.streams.remove(&conv_id) {
            existing.cancel_flag.store(true, Ordering::Relaxed);

            let token_usage = existing.token_usage.take();
            let trace_json = existing.trace_json.clone();
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
            });
        }

        let epoch = self.claim_epoch(&conv_id);
        self.streams.insert(
            conv_id.clone(),
            StreamState {
                epoch,
                status: StreamStatus::Active,
                token_usage: None,
                calls: Vec::new(),
                trace_json: None,
                task: Some(task),
                cancel_flag,
                pending_artifacts,
                has_emitted_first_chunk: false,
                pending_text: String::with_capacity(256),
                last_flush: Instant::now(),
                text_batch: None,
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
        self.ensure_flush_timer(cx);

        // Same as `register_stream`: drain both buffering layers before the
        // superseded turn's task is dropped (AGE-372).
        self.flush_text("__pending__", "__pending__", cx);

        // Cancel any existing pending stream — emit StreamEnded so subscribers
        // (app_controller) can transition Running tool calls to Cancelled.
        if let Some(mut existing) = self.streams.remove("__pending__") {
            existing.cancel_flag.store(true, Ordering::Relaxed);

            let token_usage = existing.token_usage.take();
            let trace_json = existing.trace_json.clone();

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
            });
        }

        let epoch = self.claim_epoch("__pending__");
        self.streams.insert(
            "__pending__".to_string(),
            StreamState {
                epoch,
                status: StreamStatus::Active,
                token_usage: None,
                calls: Vec::new(),
                trace_json: None,
                task: Some(task),
                cancel_flag,
                pending_artifacts,
                has_emitted_first_chunk: false,
                pending_text: String::with_capacity(256),
                last_flush: Instant::now(),
                text_batch: None,
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

    /// Flush every layer of buffered text for the stream keyed `key`,
    /// oldest first: the `DesktopSink` batch upstream of this manager is
    /// drained into `pending_text` (it holds text the sink has not forwarded
    /// yet), then everything buffered goes out as one `TextChunk`.
    ///
    /// `emit_id` is the conversation the event is emitted under; it differs
    /// from `key` only for a `__pending__` stream that has already resolved
    /// to a conversation.
    ///
    /// Draining the sink's batch is what makes this the *only* flush a
    /// caller needs: `stop_stream`, `cancel_pending` and the two supersede
    /// paths drop the turn's task right after, taking the sink with it
    /// (AGE-372), and `ensure_flush_timer` calls this so the timer
    /// backstops the layer that actually buffers.
    fn flush_text(&mut self, key: &str, emit_id: &str, cx: &mut gpui::Context<Self>) {
        let batch = self
            .streams
            .get(key)
            .and_then(|state| state.text_batch.clone());
        let drained = batch.and_then(|batch| batch.borrow_mut().drain(cx));
        if let Some(state) = self.streams.get_mut(key) {
            if let Some(text) = drained {
                state.pending_text.push_str(&text);
            }
            Self::flush_pending_text_for(state, emit_id, cx);
        }
    }

    /// Let the turn's `DesktopSink` hand this manager the buffer it fills,
    /// so `flush_text` can drain it (AGE-372). Called once per turn from
    /// `run_llm_stream`, after the stream is registered and — for a new
    /// conversation — after `promote_pending` has moved it under its real
    /// conversation ID.
    pub fn attach_text_batch(&mut self, conv_id: &str, batch: SharedTextBatch) {
        match self.streams.get_mut(conv_id) {
            Some(state) => state.text_batch = Some(batch),
            None => warn!(conv_id = %conv_id, "No registered stream to attach the text batch to"),
        }
    }

    /// Process a stream chunk: update internal state and emit the corresponding event.
    ///
    /// Text chunks use a hybrid strategy: the *first* chunk is emitted immediately
    /// (zero latency), then subsequent chunks are batched and emitted only when
    /// `FLUSH_INTERVAL` has elapsed (`should_flush_text`), backstopped by
    /// `ensure_flush_timer` for a mid-interval pause. Every other chunk type
    /// flushes any buffered text first, then is forwarded immediately without
    /// delay — otherwise a tool-start row could paint before the sentence
    /// that preceded it (AGE-166).
    pub fn handle_chunk(
        &mut self,
        conv_id: &str,
        chunk: StreamChunk,
        cx: &mut gpui::Context<Self>,
    ) {
        if !matches!(chunk, StreamChunk::Text(_)) {
            self.flush_text(conv_id, conv_id, cx);
        }
        match chunk {
            StreamChunk::Text(text) => {
                if let Some(state) = self.streams.get_mut(conv_id) {
                    state.pending_text.push_str(&text);
                    let is_first = !state.has_emitted_first_chunk;
                    if should_flush_text(is_first, state.last_flush.elapsed()) {
                        state.has_emitted_first_chunk = true;
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
                cx.emit(StreamManagerEvent::ToolCallResult {
                    conversation_id: conv_id.to_string(),
                    id,
                    result,
                });
            }
            StreamChunk::ToolCallError { id, error } => {
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
            StreamChunk::ClarificationRequested { id, questions } => {
                cx.emit(StreamManagerEvent::ClarificationRequested {
                    conversation_id: conv_id.to_string(),
                    id,
                    questions,
                });
            }
            StreamChunk::ApiCallUsage(call) => {
                if let Some(state) = self.streams.get_mut(conv_id) {
                    state.calls.push(call);
                }
            }
            StreamChunk::TurnUsage(aggregate) => {
                let ApiCallUsage {
                    input_tokens,
                    output_tokens,
                    cache_read_tokens,
                    cache_write_tokens,
                    ..
                } = aggregate;
                if let Some(state) = self.streams.get_mut(conv_id) {
                    // The per-request records are the source of truth; the
                    // provider's aggregate only stands in when none arrived.
                    let usage = if state.calls.is_empty() {
                        let mut usage = TokenUsage::new(input_tokens, output_tokens);
                        usage.cache_read_tokens = cache_read_tokens;
                        usage.cache_write_tokens = cache_write_tokens;
                        usage
                    } else {
                        TokenUsage::from_calls(std::mem::take(&mut state.calls))
                    };
                    if usage.input_tokens != input_tokens || usage.output_tokens != output_tokens {
                        warn!(
                            conv_id = %conv_id,
                            summed_input = usage.input_tokens,
                            reported_input = input_tokens,
                            summed_output = usage.output_tokens,
                            reported_output = output_tokens,
                            "Per-call usage does not sum to the provider's aggregate"
                        );
                    }
                    state.token_usage = Some(usage);
                }
                cx.emit(StreamManagerEvent::TokenUsage {
                    conversation_id: conv_id.to_string(),
                    input_tokens,
                    output_tokens,
                    cache_read_tokens,
                    cache_write_tokens,
                });
            }
            StreamChunk::TurnMessages(_) => {
                // Persisted by the conversation model (AGE-247); the UI
                // renders tool activity from the trace.
            }
            StreamChunk::Done => {
                // Don't finalize yet — caller should call finalize_stream()
            }
            StreamChunk::Error(error) => {
                // Buffered text was already flushed above (any non-text chunk
                // flushes first), so it lands before this StreamEnded.
                // StreamStatus stays a display string (AGE-244 / D5's typed
                // `kind` is for recovery decisions, made in on_chunk before
                // this point; nothing downstream of StreamManager re-classifies).
                let message = error.message;
                let (token_usage, trace_json, epoch) =
                    if let Some(state) = self.streams.get_mut(conv_id) {
                        state.status = StreamStatus::Error(message.clone());
                        (
                            state.token_usage.take(),
                            state.trace_json.clone(),
                            state.epoch,
                        )
                    } else {
                        (None, None, 0)
                    };
                cx.emit(StreamManagerEvent::StreamEnded {
                    conversation_id: conv_id.to_string(),
                    epoch,
                    status: StreamStatus::Error(message),
                    token_usage,
                    trace_json,
                    pending_artifacts: None,
                });
                self.streams.remove(conv_id);
            }
        }
    }

    /// The desktop's binding to chatty-core's turn contract (AGE-194): fold
    /// one [`SessionEvent`] into the stream's lifecycle state and emit the
    /// `StreamManagerEvent` the UI subscribes to.
    ///
    /// The manager keeps owning what is genuinely the desktop's — text
    /// batching, the epoch, the terminal `StreamEnded` — and stops owning
    /// turn orchestration: usage arrives already folded, the follow-up is
    /// the caller's to inject, and sub-agent progress and the turn's
    /// messages go to the conversation, not through here. Set the trace
    /// (`set_trace`) before passing `Error` or `TurnEnded`, as `run_llm_stream`
    /// does today: both drop the stream.
    pub fn handle_session_event(
        &mut self,
        conv_id: &str,
        event: chatty_core::session::SessionEvent,
        cx: &mut gpui::Context<Self>,
    ) {
        use chatty_core::session::SessionEvent;
        match event {
            // `StreamStarted` is emitted by `register_stream`.
            SessionEvent::TurnStarted => {}
            SessionEvent::Text(text) => self.handle_chunk(conv_id, StreamChunk::Text(text), cx),
            SessionEvent::ToolCallStarted { id, name } => {
                self.handle_chunk(conv_id, StreamChunk::ToolCallStarted { id, name }, cx)
            }
            SessionEvent::ToolCallInput { id, arguments } => {
                self.handle_chunk(conv_id, StreamChunk::ToolCallInput { id, arguments }, cx)
            }
            SessionEvent::ToolCallResult { id, result } => {
                self.handle_chunk(conv_id, StreamChunk::ToolCallResult { id, result }, cx)
            }
            SessionEvent::ToolCallError { id, error } => {
                self.handle_chunk(conv_id, StreamChunk::ToolCallError { id, error }, cx)
            }
            SessionEvent::ApprovalRequested {
                id,
                command,
                is_sandboxed,
            } => self.handle_chunk(
                conv_id,
                StreamChunk::ApprovalRequested {
                    id,
                    command,
                    is_sandboxed,
                },
                cx,
            ),
            SessionEvent::ApprovalResolved { id, approved } => {
                self.handle_chunk(conv_id, StreamChunk::ApprovalResolved { id, approved }, cx)
            }
            SessionEvent::ClarificationRequested { id, questions } => self.handle_chunk(
                conv_id,
                StreamChunk::ClarificationRequested { id, questions },
                cx,
            ),
            // Folded into `TokenUsage` by the session.
            SessionEvent::ApiCallUsage(_) => {}
            SessionEvent::TokenUsage(usage) => {
                if let Some(state) = self.streams.get_mut(conv_id) {
                    state.token_usage = Some(usage.clone());
                }
                cx.emit(StreamManagerEvent::TokenUsage {
                    conversation_id: conv_id.to_string(),
                    input_tokens: usage.input_tokens,
                    output_tokens: usage.output_tokens,
                    cache_read_tokens: usage.cache_read_tokens,
                    cache_write_tokens: usage.cache_write_tokens,
                });
            }
            // The conversation's (`AgentSession::apply`), not the manager's.
            SessionEvent::TurnMessages(_) | SessionEvent::Delegation(_) => {}
            SessionEvent::Error(error) => self.handle_chunk(conv_id, StreamChunk::Error(error), cx),
            // A cancelled turn still ends; `stop_stream` already reported a
            // user-pressed Stop, and a flag-only cancel ends as completed.
            SessionEvent::Cancelled => {}
            SessionEvent::TurnEnded => {
                // An errored stream already emitted `StreamEnded` and dropped
                // itself above.
                if self.streams.contains_key(conv_id) {
                    self.finalize_stream(conv_id, cx);
                }
            }
            // The caller's to inject as the next turn.
            SessionEvent::FollowUp(_) => {}
        }
    }

    /// Mark a stream as completed and emit StreamEnded.
    /// Called when the stream loop finishes normally.
    /// Flushes any pending batched text, then drains any pending artifacts queued by AddAttachmentTool.
    pub fn finalize_stream(&mut self, conv_id: &str, cx: &mut gpui::Context<Self>) {
        // Flush any remaining buffered text before emitting StreamEnded
        self.flush_text(conv_id, conv_id, cx);

        let (token_usage, trace_json, artifacts, epoch) =
            if let Some(state) = self.streams.get(conv_id) {
                let drained = state
                    .pending_artifacts
                    .as_ref()
                    .and_then(|pa| pa.lock().ok())
                    .map(|mut v| v.drain(..).collect::<Vec<_>>())
                    .filter(|v| !v.is_empty());
                (
                    state.token_usage.clone(),
                    state.trace_json.clone(),
                    drained,
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

        // Flush every buffered layer before the cancellation event *and*
        // before the task is dropped below: the turn's `DesktopSink` goes
        // with that task, so whatever its batch still holds would otherwise
        // never reach `Conversation.streaming_message` — which is exactly
        // what `finalize_stopped_stream` persists (AGE-372).
        self.flush_text(&key, conv_id, cx);

        if let Some(mut state) = self.streams.remove(&key) {
            // Set cancellation flag for graceful shutdown
            state.cancel_flag.store(true, Ordering::Relaxed);
            state.status = StreamStatus::Cancelled;

            let token_usage = state.token_usage.take();
            let trace_json = state.trace_json.clone();
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
            });

            // Clean up pending resolved IDs if we used the pending key
            if key == "__pending__" {
                self.pending_resolved_ids.remove("__pending__");
            }
        }
    }

    /// Cancel any pending stream (used when creating a new conversation).
    pub fn cancel_pending(&mut self, cx: &mut gpui::Context<Self>) {
        // As in `stop_stream`: both buffering layers, before the task goes.
        self.flush_text("__pending__", "__pending__", cx);

        if let Some(state) = self.streams.remove("__pending__") {
            state.cancel_flag.store(true, Ordering::Relaxed);
            debug!("Cancelled pending stream");
            cx.emit(StreamManagerEvent::StreamEnded {
                conversation_id: "__pending__".to_string(),
                epoch: state.epoch,
                status: StreamStatus::Cancelled,
                token_usage: state.token_usage,
                trace_json: state.trace_json,
                pending_artifacts: None,
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
            // Both buffering layers, before the task goes — same reason as
            // `stop_stream` (AGE-372).
            self.flush_text(&key, &key, cx);

            if let Some(state) = self.streams.remove(&key) {
                state.cancel_flag.store(true, Ordering::Relaxed);
                cx.emit(StreamManagerEvent::StreamEnded {
                    conversation_id: key,
                    epoch: state.epoch,
                    status: StreamStatus::Cancelled,
                    token_usage: state.token_usage,
                    trace_json: state.trace_json,
                    pending_artifacts: None,
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
    use std::cell::RefCell;
    use std::rc::Rc;

    use gpui::AppContext as _;

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
            calls: Vec::new(),
            trace_json: None,
            task: None,
            cancel_flag: Arc::new(AtomicBool::new(false)),
            pending_artifacts: None,
            has_emitted_first_chunk: false,
            pending_text: String::new(),
            last_flush: Instant::now(),
            text_batch: None,
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
        mgr.streams
            .insert("conv-1".to_string(), active_state(first));
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
                calls: Vec::new(),
                trace_json: None,
                task: None,
                cancel_flag: Arc::new(AtomicBool::new(false)),
                pending_artifacts: None,
                has_emitted_first_chunk: false,
                pending_text: String::new(),
                last_flush: Instant::now(),
                text_batch: None,
            },
        );
        assert!(mgr.is_streaming("conv-123"));
        assert!(!mgr.is_streaming("other"));
    }

    #[test]
    fn test_promote_pending() {
        let mut mgr = StreamManager::new();
        let epoch = mgr.claim_epoch("__pending__");
        mgr.streams
            .insert("__pending__".to_string(), active_state(epoch));
        mgr.pending_resolved_ids.insert(
            "__pending__".to_string(),
            Arc::new(Mutex::new(Some("conv-456".to_string()))),
        );

        mgr.promote_pending("conv-456");

        assert!(!mgr.streams.contains_key("__pending__"));
        assert!(mgr.streams.contains_key("conv-456"));
        assert!(mgr.pending_resolved_ids.is_empty());
    }

    #[test]
    fn test_set_trace() {
        let mut mgr = StreamManager::new();
        mgr.streams.insert("conv-1".to_string(), active_state(1));

        let trace = serde_json::json!({"tool_calls": []});
        mgr.set_trace("conv-1", Some(trace.clone()));

        assert_eq!(mgr.streams.get("conv-1").unwrap().trace_json, Some(trace));
    }

    // -------------------------------------------------------------------
    // AGE-166: raise the TextChunk flush interval to ~16-33ms, add a timer
    // flush, and flush pending text before any non-text event.
    // -------------------------------------------------------------------

    #[test]
    fn flush_interval_is_in_the_16_to_33ms_display_refresh_band() {
        assert!(
            FLUSH_INTERVAL >= Duration::from_millis(16)
                && FLUSH_INTERVAL <= Duration::from_millis(33),
            "FLUSH_INTERVAL={FLUSH_INTERVAL:?} should sit in the ~16-33ms band a 30-60Hz \
             display can actually paint"
        );
    }

    // -------------------------------------------------------------------
    // TextBatch (AGE-166 / AGE-372): the one buffer coalescing raw
    // SessionEvent::Text chunks ahead of `update_global` /
    // `StreamManager::handle_chunk`. `push`/`take` are the pure half of it;
    // `drain` (which also applies to the session) is exercised end to end by
    // the `DesktopSink` tests in `message_ops_internals.rs`.
    // -------------------------------------------------------------------

    /// The very first chunk of a turn must flush immediately — batching must
    /// never delay time-to-first-token.
    #[test]
    fn text_batch_flushes_the_first_chunk_immediately() {
        let mut batch = TextBatch::new("conv-1".to_string());
        assert!(batch.push("Hello"));
        assert_eq!(batch.take().as_deref(), Some("Hello"));
    }

    /// A flood of chunks arriving faster than `FLUSH_INTERVAL` apart must
    /// coalesce into a single pending buffer rather than flushing each one —
    /// this is the "one update_global + one handle_chunk per flush interval"
    /// requirement (AGE-166 remediation #2), at the unit level: `push`
    /// answering `false` means neither call happens for that chunk.
    #[test]
    fn text_batch_coalesces_a_flood_after_the_first_chunk() {
        let mut batch = TextBatch::new("conv-1".to_string());
        assert!(batch.push("a"), "first chunk flushes immediately");
        batch.take();
        for _ in 0..999 {
            assert!(
                !batch.push("x"),
                "chunk arriving well within FLUSH_INTERVAL of the last flush must buffer, not flush"
            );
        }
        // Nothing was lost — it's all still sitting in the pending buffer.
        assert_eq!(batch.pending.len(), 999);
    }

    /// Once `FLUSH_INTERVAL` has actually elapsed, the next chunk flushes
    /// the whole accumulated buffer.
    #[test]
    fn text_batch_flushes_once_flush_interval_has_elapsed() {
        let mut batch = TextBatch::new("conv-1".to_string());
        assert!(batch.push("first")); // consumes the immediate-first-chunk flush
        batch.take();
        batch.pending.push_str("buffered");
        batch.last_flush = Instant::now() - (FLUSH_INTERVAL * 2);

        assert!(batch.push(" more"));
        assert_eq!(batch.take().as_deref(), Some("buffered more"));
    }

    /// `take()` (used before any non-text event, and by every drain path)
    /// drains whatever is buffered regardless of how much time has passed,
    /// and is a no-op when there's nothing pending — so flushing before a
    /// tool call never emits a spurious empty TextChunk.
    #[test]
    fn text_batch_take_drains_regardless_of_elapsed_time() {
        let mut batch = TextBatch::new("conv-1".to_string());
        assert_eq!(batch.take(), None, "nothing buffered yet");

        batch.push("first");
        assert_eq!(batch.take().as_deref(), Some("first"));
        batch.pending.push_str("not yet flushed");
        assert_eq!(batch.take().as_deref(), Some("not yet flushed"));
        assert_eq!(batch.take(), None, "draining twice must not re-emit");
    }

    #[test]
    fn should_flush_text_is_true_for_the_first_chunk_regardless_of_elapsed_time() {
        assert!(should_flush_text(true, Duration::ZERO));
    }

    #[test]
    fn should_flush_text_waits_for_the_interval_after_the_first_chunk() {
        assert!(!should_flush_text(false, Duration::ZERO));
        assert!(!should_flush_text(false, FLUSH_INTERVAL / 2));
        assert!(should_flush_text(false, FLUSH_INTERVAL));
        assert!(should_flush_text(false, FLUSH_INTERVAL * 2));
    }

    /// Register a stream, subscribe to every `StreamManagerEvent` it emits,
    /// and return the entity plus the running capture.
    fn subscribed_manager(
        cx: &mut gpui::TestAppContext,
    ) -> (
        gpui::Entity<StreamManager>,
        Rc<RefCell<Vec<StreamManagerEvent>>>,
    ) {
        let manager = cx.update(|cx| cx.new(|_cx| StreamManager::new()));
        let events: Rc<RefCell<Vec<StreamManagerEvent>>> = Rc::default();
        let sink = events.clone();
        cx.update(|cx| {
            cx.subscribe(&manager, move |_mgr, event: &StreamManagerEvent, _cx| {
                sink.borrow_mut().push(event.clone());
            })
            .detach();
        });
        (manager, events)
    }

    /// Under a token flood arriving faster than `FLUSH_INTERVAL`, the
    /// emitted `TextChunk` rate must stay near the ~60/s budget — not the
    /// once-per-raw-chunk rate a naive forward would produce. This can't
    /// rely on wall-clock sleeping in a test (the flush gate reads real
    /// `Instant::now()`), so the flood is sent with no real time elapsing
    /// between chunks (worst case for a naive implementation) and the
    /// simulated clock is advanced to let the periodic flush timer do the
    /// rate-limiting — the same backstop a real sustained flood relies on
    /// when tokens arrive faster than the timer.
    ///
    /// The step size is a **literal** 1ms, deliberately not derived from
    /// `FLUSH_INTERVAL`: an earlier version of this test advanced the clock
    /// in `FLUSH_INTERVAL`-sized steps for a fixed number of iterations,
    /// which summed to "1 simulated second" only because 50 × 20ms = 1s by
    /// construction — the assertion was really "≤60 flushes per 50 loop
    /// iterations", true for *any* interval, so reverting `FLUSH_INTERVAL`
    /// to the pre-fix 5ms (200/s) still passed. A 1ms step is far finer
    /// than anything in the issue's own ~16-33ms band, so however often the
    /// *real* `FLUSH_INTERVAL` constant actually fires within this literal
    /// 1-second window is what gets counted below (verified: reverting to
    /// 5ms here does fail this assertion, at ~200 emitted TextChunks).
    #[gpui::test]
    async fn text_chunk_rate_stays_capped_under_a_token_flood(cx: &mut gpui::TestAppContext) {
        let (manager, events) = subscribed_manager(cx);
        let cancel_flag = Arc::new(AtomicBool::new(false));
        cx.update(|cx| {
            manager.update(cx, |mgr, cx| {
                let task = cx.background_executor().spawn(async { Ok(()) });
                mgr.register_stream("conv-flood".into(), task, cancel_flag, None, cx);
            });
        });

        const STEP: Duration = Duration::from_millis(1);
        const STEPS: u32 = 1000; // 1000 * 1ms literal = 1 simulated second
        const CHUNKS_PER_STEP: u32 = 5; // 5000 raw chunks/s — a real flood
        for _ in 0..STEPS {
            cx.update(|cx| {
                manager.update(cx, |mgr, cx| {
                    for _ in 0..CHUNKS_PER_STEP {
                        mgr.handle_chunk("conv-flood", StreamChunk::Text("x".into()), cx);
                    }
                });
            });
            cx.executor().advance_clock(STEP);
            cx.run_until_parked();
        }

        let text_chunk_count = events
            .borrow()
            .iter()
            .filter(|e| matches!(e, StreamManagerEvent::TextChunk { .. }))
            .count();
        eprintln!(
            "AGE-166 measured: {text_chunk_count} TextChunk events for {} raw chunks over \
             1 simulated second at FLUSH_INTERVAL={FLUSH_INTERVAL:?} \
             (run with --nocapture to see this on a passing run)",
            STEPS * CHUNKS_PER_STEP
        );
        // 60 is the acceptance criterion's own number. Real observed count
        // at FLUSH_INTERVAL=20ms is ~51 (1 immediate + ~50 periodic); the
        // gap to 60 is this test's margin against wall-clock creep on a
        // loaded runner nudging a handful of per-chunk immediate flushes
        // (state.last_flush.elapsed(), real time) past the periodic timer's
        // own (simulated-clock) cadence.
        assert!(
            text_chunk_count <= 60,
            "TextChunk emitted {text_chunk_count} times over one simulated second of a \
             {CHUNKS_PER_STEP}-chunk-per-ms token flood ({} raw chunks); want <= ~60/s",
            STEPS * CHUNKS_PER_STEP
        );
    }

    /// A pause mid-interval (no further chunks) must still paint: the
    /// background flush timer, not just the per-chunk elapsed check, is what
    /// flushes it.
    #[gpui::test]
    async fn a_pause_mid_interval_is_flushed_by_the_timer(cx: &mut gpui::TestAppContext) {
        let (manager, events) = subscribed_manager(cx);
        let cancel_flag = Arc::new(AtomicBool::new(false));
        cx.update(|cx| {
            manager.update(cx, |mgr, cx| {
                let task = cx.background_executor().spawn(async { Ok(()) });
                mgr.register_stream("conv-pause".into(), task, cancel_flag, None, cx);
                // First chunk flushes immediately; second is buffered and
                // would sit there until the next chunk without the timer.
                mgr.handle_chunk("conv-pause", StreamChunk::Text("Working".into()), cx);
                mgr.handle_chunk("conv-pause", StreamChunk::Text(" on it".into()), cx);
            });
        });

        assert_eq!(
            events
                .borrow()
                .iter()
                .filter(|e| matches!(e, StreamManagerEvent::TextChunk { .. }))
                .count(),
            1,
            "only the immediate first-chunk flush should have happened yet"
        );

        // No further chunk arrives — just let simulated time pass.
        cx.executor().advance_clock(FLUSH_INTERVAL * 2);
        cx.run_until_parked();

        let text_chunks: Vec<String> = events
            .borrow()
            .iter()
            .filter_map(|e| match e {
                StreamManagerEvent::TextChunk { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            text_chunks,
            vec!["Working".to_string(), " on it".to_string()],
            "the paused chunk must be painted by the timer, not stranded until the next token"
        );
    }

    /// A tool-start event must never be visible before the buffered text
    /// that preceded it: `handle_chunk` flushes pending text before any
    /// non-text chunk.
    #[gpui::test]
    async fn tool_start_never_precedes_its_buffered_text(cx: &mut gpui::TestAppContext) {
        let (manager, events) = subscribed_manager(cx);
        let cancel_flag = Arc::new(AtomicBool::new(false));
        cx.update(|cx| {
            manager.update(cx, |mgr, cx| {
                let task = cx.background_executor().spawn(async { Ok(()) });
                mgr.register_stream("conv-order".into(), task, cancel_flag, None, cx);
                // First chunk flushes immediately (has_emitted_first_chunk
                // becomes true); the second arrives well within
                // FLUSH_INTERVAL and stays buffered — with no
                // flush-before-non-text-event fix, the tool-start below
                // would be visible before this second chunk of text.
                mgr.handle_chunk("conv-order", StreamChunk::Text("Let me".into()), cx);
                mgr.handle_chunk("conv-order", StreamChunk::Text(" check that.".into()), cx);
                mgr.handle_chunk(
                    "conv-order",
                    StreamChunk::ToolCallStarted {
                        id: "call-1".into(),
                        name: "read_file".into(),
                    },
                    cx,
                );
            });
        });

        let kinds: Vec<&str> = events
            .borrow()
            .iter()
            .map(|e| match e {
                StreamManagerEvent::StreamStarted { .. } => "StreamStarted",
                StreamManagerEvent::TextChunk { .. } => "TextChunk",
                StreamManagerEvent::ToolCallStarted { .. } => "ToolCallStarted",
                _ => "other",
            })
            .collect();
        assert_eq!(
            kinds,
            vec!["StreamStarted", "TextChunk", "TextChunk", "ToolCallStarted"],
            "buffered text must flush before the tool call that followed it"
        );
    }

    /// `ensure_flush_timer` must not spin at `FLUSH_INTERVAL` on the GPUI
    /// foreground executor for the rest of the process once every stream
    /// has ended — it should stop and let the next stream respawn it.
    #[gpui::test]
    async fn flush_timer_stops_once_no_streams_remain(cx: &mut gpui::TestAppContext) {
        let (manager, _events) = subscribed_manager(cx);
        let cancel_flag = Arc::new(AtomicBool::new(false));
        cx.update(|cx| {
            manager.update(cx, |mgr, cx| {
                let task = cx.background_executor().spawn(async { Ok(()) });
                mgr.register_stream("conv-solo".into(), task, cancel_flag, None, cx);
                assert!(
                    mgr.flush_timer.is_some(),
                    "registering a stream must start the background timer"
                );
                mgr.finalize_stream("conv-solo", cx);
            });
        });

        // Let the timer's next tick observe the now-empty stream map and
        // stop itself.
        cx.executor().advance_clock(FLUSH_INTERVAL);
        cx.run_until_parked();

        manager.update(cx, |mgr, _cx| {
            assert!(
                mgr.flush_timer.is_none(),
                "the background timer must stop once no stream remains, not spin forever"
            );
        });
    }

    /// AGE-166 acceptance criterion #4 substitute: `samply`/`cargo-flamegraph`
    /// are not installed in this sandbox and `perf_event_paranoid` blocks
    /// unprivileged sampling, so a real flamegraph of a live LLM stream
    /// driving the actual desktop window isn't obtainable here. This
    /// measures the two numbers the issue actually asks for — the emitted
    /// TextChunk notify rate, and `handle_chunk` / `flush_pending_text`
    /// self-time — directly, with a wall-clock timing wrapper around the
    /// same flood `text_chunk_rate_stays_capped_under_a_token_flood` drives,
    /// run in `--release` so the numbers aren't inflated by debug
    /// assertions. `#[ignore]`d: a timing measurement, not a correctness
    /// assertion fit for every CI run.
    ///
    /// Run: `cargo test --release -p chatty-gpui --all-features -- \
    /// --ignored --nocapture age_166_measured_self_time_and_notify_rate`
    #[gpui::test]
    #[ignore]
    async fn age_166_measured_self_time_and_notify_rate(cx: &mut gpui::TestAppContext) {
        let (manager, events) = subscribed_manager(cx);
        let cancel_flag = Arc::new(AtomicBool::new(false));
        cx.update(|cx| {
            manager.update(cx, |mgr, cx| {
                let task = cx.background_executor().spawn(async { Ok(()) });
                mgr.register_stream("conv-profile".into(), task, cancel_flag, None, cx);
            });
        });

        // handle_chunk self-time under sustained real-world load: chunks
        // arrive faster than any real display refresh, so almost every call
        // takes the cheap buffer-only path (should_flush_text says no) —
        // that's exactly the shape the rate test's flood exercises, and
        // timing it directly with real Instant::now() around each call
        // gives wall time actually spent inside handle_chunk, not a
        // synthetic count.
        const RAW_CHUNKS: u32 = 200_000;
        let mut handle_chunk_total = Duration::ZERO;
        cx.update(|cx| {
            manager.update(cx, |mgr, cx| {
                for _ in 0..RAW_CHUNKS {
                    let start = Instant::now();
                    mgr.handle_chunk("conv-profile", StreamChunk::Text("x".into()), cx);
                    handle_chunk_total += start.elapsed();
                }
            });
        });
        let notify_count = events
            .borrow()
            .iter()
            .filter(|e| matches!(e, StreamManagerEvent::TextChunk { .. }))
            .count();

        // flush_pending_text self-time in isolation: same call, always with
        // something real to flush, so this isn't measuring an empty no-op.
        const FLUSH_CALLS: u32 = 200_000;
        let mut flush_total = Duration::ZERO;
        cx.update(|cx| {
            manager.update(cx, |mgr, cx| {
                for _ in 0..FLUSH_CALLS {
                    if let Some(state) = mgr.streams.get_mut("conv-profile") {
                        state.pending_text.push_str("some buffered text to flush");
                    }
                    let start = Instant::now();
                    mgr.flush_text("conv-profile", "conv-profile", cx);
                    flush_total += start.elapsed();
                }
            });
        });

        eprintln!(
            "AGE-166 measured (release build):\n\
             \x20\x20handle_chunk self-time:       {RAW_CHUNKS} calls, {handle_chunk_total:?} \
             total, {:.1} ns/call avg (a tight, no-clock-advance flood, so all but a \
             handful of calls take the cheap buffer-only path — {notify_count} of these \
             {RAW_CHUNKS} calls actually flushed/emitted a TextChunk)\n\
             \x20\x20flush_text self-time:         {FLUSH_CALLS} calls, {flush_total:?} \
             total, {:.1} ns/call avg (each call has real text to flush, so this is the \
             cx.emit path's own cost, isolated)\n\
             \x20\x20TextChunk notify rate under a sustained flood (separate test, \
             simulated-clock-driven): see \
             text_chunk_rate_stays_capped_under_a_token_flood — its own printed/asserted \
             count is the measured rate at FLUSH_INTERVAL={FLUSH_INTERVAL:?}",
            handle_chunk_total.as_nanos() as f64 / RAW_CHUNKS as f64,
            flush_total.as_nanos() as f64 / FLUSH_CALLS as f64,
        );
    }
}
