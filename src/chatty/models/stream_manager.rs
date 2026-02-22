use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use gpui::{Entity, EventEmitter, Global, Task};
use tracing::{debug, warn};

use crate::chatty::services::StreamChunk;

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
    pub status: StreamStatus,
    pub token_usage: Option<(u32, u32)>,
    pub trace_json: Option<serde_json::Value>,
    task: Option<Task<anyhow::Result<()>>>,
    cancel_flag: Arc<AtomicBool>,
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
        status: StreamStatus,
        token_usage: Option<(u32, u32)>,
        trace_json: Option<serde_json::Value>,
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
}

impl EventEmitter<StreamManagerEvent> for StreamManager {}

impl StreamManager {
    pub fn new() -> Self {
        Self {
            streams: HashMap::new(),
            pending_resolved_ids: HashMap::new(),
        }
    }

    /// Register a stream for a known conversation.
    /// If a stream already exists for this conversation, it is cancelled first.
    pub fn register_stream(
        &mut self,
        conv_id: String,
        task: Task<anyhow::Result<()>>,
        cancel_flag: Arc<AtomicBool>,
        cx: &mut gpui::Context<Self>,
    ) {
        // Cancel existing stream if any
        if let Some(existing) = self.streams.remove(&conv_id) {
            existing.cancel_flag.store(true, Ordering::Relaxed);
            debug!(conv_id = %conv_id, "Cancelled existing stream before registering new one");
        }

        self.streams.insert(
            conv_id.clone(),
            StreamState {
                status: StreamStatus::Active,
                token_usage: None,
                trace_json: None,
                task: Some(task),
                cancel_flag,
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
        cx: &mut gpui::Context<Self>,
    ) {
        // Cancel any existing pending stream
        if let Some(existing) = self.streams.remove("__pending__") {
            existing.cancel_flag.store(true, Ordering::Relaxed);
            debug!("Cancelled existing pending stream");
        }

        self.streams.insert(
            "__pending__".to_string(),
            StreamState {
                status: StreamStatus::Active,
                token_usage: None,
                trace_json: None,
                task: Some(task),
                cancel_flag,
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
            self.streams.insert(conv_id.to_string(), state);
        }
        self.pending_resolved_ids.remove("__pending__");
    }

    /// Process a stream chunk: update internal state and emit the corresponding event.
    ///
    /// Note: Text chunks are emitted as events for UI updates but NOT accumulated
    /// in StreamManager. Text accumulation happens in `ConversationsStore.streaming_message`.
    pub fn handle_chunk(
        &mut self,
        conv_id: &str,
        chunk: StreamChunk,
        cx: &mut gpui::Context<Self>,
    ) {
        match chunk {
            StreamChunk::Text(text) => {
                cx.emit(StreamManagerEvent::TextChunk {
                    conversation_id: conv_id.to_string(),
                    text,
                });
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
                if let Some(state) = self.streams.get_mut(conv_id) {
                    state.status = StreamStatus::Error(error.clone());
                }
                let (token_usage, trace_json) = if let Some(state) = self.streams.get(conv_id) {
                    (state.token_usage, state.trace_json.clone())
                } else {
                    (None, None)
                };
                cx.emit(StreamManagerEvent::StreamEnded {
                    conversation_id: conv_id.to_string(),
                    status: StreamStatus::Error(error),
                    token_usage,
                    trace_json,
                });
                self.streams.remove(conv_id);
            }
        }
    }

    /// Mark a stream as completed and emit StreamEnded.
    /// Called when the stream loop finishes normally.
    pub fn finalize_stream(&mut self, conv_id: &str, cx: &mut gpui::Context<Self>) {
        let (token_usage, trace_json) = if let Some(state) = self.streams.get(conv_id) {
            (state.token_usage, state.trace_json.clone())
        } else {
            warn!(conv_id = %conv_id, "finalize_stream called but no stream found");
            return;
        };

        cx.emit(StreamManagerEvent::StreamEnded {
            conversation_id: conv_id.to_string(),
            status: StreamStatus::Completed,
            token_usage,
            trace_json,
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
            // Set cancellation flag for graceful shutdown
            state.cancel_flag.store(true, Ordering::Relaxed);
            state.status = StreamStatus::Cancelled;

            let token_usage = state.token_usage;
            let trace_json = state.trace_json.clone();

            debug!(conv_id = %conv_id, "Stream stopped gracefully");

            // Drop the task (backstop — the cancel flag should cause clean exit)
            drop(state.task.take());

            cx.emit(StreamManagerEvent::StreamEnded {
                conversation_id: conv_id.to_string(),
                status: StreamStatus::Cancelled,
                token_usage,
                trace_json,
            });

            // Clean up pending resolved IDs if we used the pending key
            if key == "__pending__" {
                self.pending_resolved_ids.remove("__pending__");
            }
        }
    }

    /// Cancel any pending stream (used when creating a new conversation).
    pub fn cancel_pending(&mut self, cx: &mut gpui::Context<Self>) {
        if let Some(state) = self.streams.remove("__pending__") {
            state.cancel_flag.store(true, Ordering::Relaxed);
            debug!("Cancelled pending stream");
            cx.emit(StreamManagerEvent::StreamEnded {
                conversation_id: "__pending__".to_string(),
                status: StreamStatus::Cancelled,
                token_usage: state.token_usage,
                trace_json: state.trace_json,
            });
        }
        self.pending_resolved_ids.remove("__pending__");
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
            if let Some(state) = self.streams.remove(&key) {
                state.cancel_flag.store(true, Ordering::Relaxed);
                cx.emit(StreamManagerEvent::StreamEnded {
                    conversation_id: key,
                    status: StreamStatus::Cancelled,
                    token_usage: state.token_usage,
                    trace_json: state.trace_json,
                });
            }
        }
        self.pending_resolved_ids.clear();
    }
}

/// Global accessor for the StreamManager entity.
/// Stores a strong `Entity` reference to prevent the StreamManager from being
/// garbage collected when the initialization closure's local variables go out of scope.
pub struct GlobalStreamManager {
    pub entity: Option<Entity<StreamManager>>,
}

impl Global for GlobalStreamManager {}

#[cfg(test)]
mod tests {
    use super::*;

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
                status: StreamStatus::Active,
                token_usage: None,
                trace_json: None,
                task: None,
                cancel_flag: Arc::new(AtomicBool::new(false)),
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
                status: StreamStatus::Active,
                token_usage: None,
                trace_json: None,
                task: None,
                cancel_flag: Arc::new(AtomicBool::new(false)),
            },
        );
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
        mgr.streams.insert(
            "conv-1".to_string(),
            StreamState {
                status: StreamStatus::Active,
                token_usage: None,
                trace_json: None,
                task: None,
                cancel_flag: Arc::new(AtomicBool::new(false)),
            },
        );

        let trace = serde_json::json!({"tool_calls": []});
        mgr.set_trace("conv-1", Some(trace.clone()));

        assert_eq!(mgr.streams.get("conv-1").unwrap().trace_json, Some(trace));
    }
}
