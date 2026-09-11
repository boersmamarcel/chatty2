//! Internal helpers extracted from `message_ops.rs` to keep that file
//! under ~1300 LOC. All items are `pub(super)` and only used by
//! `message_ops.rs` siblings of this file.
//!
//! See `message_ops.rs` for the high-level `ChattyApp` methods that
//! orchestrate these helpers.

use std::time::Instant;

use super::*;
use crate::chatty::models::stream_manager::should_flush_text;
use chatty_core::tools::invoke_agent_tool::InvokeAgentProgress;

/// Parameters for one turn on the desktop.
pub(super) struct LlmStreamParams {
    pub(super) conv_id: String,
    pub(super) input: TurnInput,
    pub(super) chat_view: Entity<ChatView>,
    pub(super) stream_manager: Option<Entity<crate::chatty::models::StreamManager>>,
    /// The token `StreamManager` was registered with, so a Stop reaches the
    /// turn: the session runs under the same flag.
    pub(super) cancel_flag: Arc<AtomicBool>,
    /// Weak controller handle — used to inject the follow-up the session
    /// queues (todo protocol, loop guard, malformed-call retry).
    pub(super) weak_ctrl: gpui::WeakEntity<ChattyApp>,
}

/// Run one turn of `conv_id` through its `AgentSession` and pump every
/// [`SessionEvent`] into the desktop (AGE-195).
///
/// The session owns the turn: approval channels, context shaping,
/// `stream_prompt`, the loop, the protocol follow-up, usage folding. What
/// stays here is the desktop's: the token budget snapshot, the trace, the
/// sub-agent row, the plan strip, `StreamManager`, and sending the follow-up
/// as the next turn. Finalization happens in `handle_stream_manager_event`
/// when `StreamManager` reports the turn ended; an errored turn is reported
/// the same way, from the `Error` event.
///
/// Callers are responsible for their own preamble (conversation creation, UI
/// message addition, DPO recording, etc.) and for registering the returned
/// task with StreamManager.
pub(super) async fn run_llm_stream(
    params: LlmStreamParams,
    cx: &mut AsyncApp,
) -> anyhow::Result<()> {
    let LlmStreamParams {
        conv_id,
        input,
        chat_view,
        stream_manager,
        cancel_flag,
        weak_ctrl,
    } = params;

    // 1. Token budget snapshot, computed from the history as it stands before
    //    this turn, in parallel with the LLM call.
    let user_contents = input.contents.clone();
    let history = cx
        .update_global::<ConversationsStore, _>(|store, _cx| {
            store.get_conversation(&conv_id).map(|conv| conv.messages())
        })
        .map_err(|e| warn!(error = ?e, "Failed to read history for the token budget"))
        .ok()
        .flatten()
        .unwrap_or_default();
    // 2b. Compute token budget snapshot in parallel with the LLM call.
    //
    // gather_snapshot_inputs() must run on the GPUI thread (reads globals, warms the
    // static cache), so we call it synchronously here.  The expensive part —
    // BPE-counting history and the user message — is handed off to a detached
    // cx.spawn task so stream_prompt() starts immediately on the next line without
    // waiting for the count to finish.  The bar simply shows the new snapshot on
    // whatever repaint follows the count completing (~1–10 ms later).
    {
        let user_message_text_for_budget = extract_user_message_text(&user_contents);
        let conv_id_for_budget = conv_id.clone();

        // `history` is only borrowed here: `gather_snapshot_inputs` clones it
        // (once) itself, and only on the success path, so a conversation or
        // model lookup miss no longer wastes a full history clone (finding
        // B1, AGE-219).
        let budget_inputs = cx
            .update(|cx| {
                gather_snapshot_inputs(
                    &conv_id_for_budget,
                    user_message_text_for_budget,
                    &history,
                    cx,
                )
            })
            .map_err(|e| warn!(error = ?e, "Failed to gather token budget snapshot inputs"))
            .ok()
            .flatten();

        if let Some(inputs) = budget_inputs {
            // Clone the watch::Sender out of the global before spawning.
            // watch::Sender::send() is &self, so no GPUI context is needed
            // inside the task — just the sender and the optional settings.
            let sender = cx
                .update(|cx| {
                    cx.try_global::<GlobalTokenBudget>()
                        .map(|g| g.sender.clone())
                })
                .map_err(|e| warn!(error = ?e, "Failed to read token budget sender global"))
                .ok()
                .flatten();

            let settings = cx
                .update(|cx| {
                    cx.try_global::<crate::settings::models::TokenTrackingSettings>()
                        .cloned()
                })
                .map_err(|e| warn!(error = ?e, "Failed to read token tracking settings global"))
                .ok()
                .flatten();

            // tokio::spawn runs in parallel with stream_prompt below.
            // The bar will update on whichever repaint follows the count
            // completing (~1–10 ms), while the LLM call is already in flight.
            tokio::spawn(async move {
                match compute_snapshot_background(inputs).await {
                    Ok(snapshot) => {
                        check_pressure(&snapshot, settings.as_ref());
                        if let Some(ref sender) = sender {
                            let _ = sender.send(Some(snapshot));
                        }
                    }
                    Err(e) => {
                        warn!(error = ?e, "Token budget snapshot computation failed (non-fatal)");
                    }
                }
            });
        }
    }

    // 2. Begin the turn on the conversation's session and pump its events.
    let mut sink = DesktopSink {
        conv_id: conv_id.clone(),
        cx: cx.clone(),
        chat_view,
        stream_manager,
        weak_ctrl,
        text_batch: TextBatch::new(),
    };
    debug!(conv_id = %conv_id, "Beginning turn on the session");
    let turn = cx
        .update_global::<ConversationsStore, _>(|store, _cx| {
            let (session, hosted) = store
                .turn_targets(&conv_id)
                .ok_or_else(|| anyhow::anyhow!("Conversation not found for the turn"))?;
            // The one line that differs for a conversation running online:
            // who opens the stream. The sink below, the `apply` into the local
            // conversation and the finalize are the same either way, because
            // the wire is a serialization of `SessionEvent` (AGE-298).
            turn_transport::begin_turn(session, hosted, input, cancel_flag, move |event| {
                sink.handle(event)
            })
        })
        .map_err(|e| anyhow::anyhow!(e.to_string()))??;
    turn.await;
    Ok(())
}

/// The desktop's half of the session seam: one [`SessionEvent`] in, the
/// conversation model, `StreamManager` and the views updated.
///
/// Every event goes to two places, in this order: the `Conversation` model
/// through `AgentSession::apply`, which is the source of truth for a stream
/// whose conversation is not on screen, and then `StreamManager`, which
/// emits the `StreamManagerEvent` the UI subscribes to. Holds its own
/// [`AsyncApp`], which is why nothing here may be `Send`.
///
/// Raw `SessionEvent::Text` chunks are buffered here (`text_batch`) rather
/// than applied one at a time: a raw LLM stream can yield far faster than any
/// display repaints, and every chunk previously paid its own `update_global`
/// into the conversation *and* its own `StreamManager` entity update even
/// though only the emitted `TextChunk` UI event was ever batched (AGE-166).
/// `handle`/`flush_text` coalesce however many chunks arrive within one
/// `FLUSH_INTERVAL` into a single pair of calls. Any non-text event flushes
/// first, so the conversation's `text_before` bookkeeping and the UI's paint
/// order both see buffered text before the tool call / approval / turn-end
/// that followed it.
struct DesktopSink {
    conv_id: String,
    cx: AsyncApp,
    chat_view: Entity<ChatView>,
    stream_manager: Option<Entity<crate::chatty::models::StreamManager>>,
    weak_ctrl: gpui::WeakEntity<ChattyApp>,
    text_batch: TextBatch,
}

/// Upstream text-batching state for `DesktopSink`, factored out of it (no
/// GPUI types) so its flush policy is unit-testable without a full
/// `AsyncApp`/`ChatView` harness (AGE-166). Shares `should_flush_text` with
/// `StreamManager`'s own UI-facing batching, so the two hot paths this issue
/// coalesces implement one flush policy, not two.
struct TextBatch {
    pending: String,
    has_flushed_first: bool,
    last_flush: Instant,
}

impl TextBatch {
    fn new() -> Self {
        Self {
            pending: String::new(),
            has_flushed_first: false,
            last_flush: Instant::now(),
        }
    }

    /// Buffer `text`; returns the batch to flush once `should_flush_text`
    /// says so (the turn's first chunk immediately, then at most once per
    /// `FLUSH_INTERVAL`), or `None` while still buffering.
    fn push(&mut self, text: &str) -> Option<String> {
        self.pending.push_str(text);
        let is_first = !self.has_flushed_first;
        if should_flush_text(is_first, self.last_flush.elapsed()) {
            self.has_flushed_first = true;
            self.last_flush = Instant::now();
            Some(std::mem::take(&mut self.pending))
        } else {
            None
        }
    }

    /// Take whatever is buffered regardless of the flush policy — used
    /// before any non-text event so nothing is left stranded.
    fn take(&mut self) -> Option<String> {
        if self.pending.is_empty() {
            return None;
        }
        self.last_flush = Instant::now();
        Some(std::mem::take(&mut self.pending))
    }
}

impl DesktopSink {
    fn handle(&mut self, event: SessionEvent) {
        if let SessionEvent::Text(text) = &event {
            if let Some(batch) = self.text_batch.push(text) {
                self.apply_and_forward_text(batch);
            }
            return;
        }

        // A non-text event must see already-buffered text applied first —
        // see the struct docs (AGE-166).
        self.flush_text();

        let conv_id = self.conv_id.clone();
        let todo_snapshot = self
            .cx
            .update_global::<ConversationsStore, _>(|store, _cx| {
                store
                    .get_session_mut(&conv_id)
                    .and_then(|session| session.apply(&event))
            })
            .map_err(|e| warn!(error = ?e, conv_id = %conv_id, "Failed to apply session event"))
            .ok()
            .flatten();
        if let Some(snapshot) = todo_snapshot {
            self.publish_todo_snapshot(snapshot);
        }

        match event {
            SessionEvent::Delegation(progress) => self.on_progress(progress),
            SessionEvent::FollowUp(prompt) => self.inject_follow_up(prompt),
            SessionEvent::Error(_) => {
                // The manager drops the stream on an error, so the trace has
                // to be attached first or the failed turn loses its tool calls.
                self.set_trace();
                self.forward(event);
            }
            SessionEvent::TurnEnded => {
                self.set_trace();
                self.forward(event);
                self.publish_skipped_verification();
            }
            event => self.forward(event),
        }
    }

    /// Flush whatever text is buffered, if any. A no-op when nothing is
    /// pending; called unconditionally before every non-text event.
    fn flush_text(&mut self) {
        if let Some(batch) = self.text_batch.take() {
            self.apply_and_forward_text(batch);
        }
    }

    /// Apply one coalesced batch of text to the session (`update_global`)
    /// and forward it to `StreamManager`, as a single `SessionEvent::Text`.
    fn apply_and_forward_text(&mut self, batch: String) {
        let conv_id = self.conv_id.clone();
        let event = SessionEvent::Text(batch);
        self.cx
            .update_global::<ConversationsStore, _>(|store, _cx| {
                store
                    .get_session_mut(&conv_id)
                    .and_then(|session| session.apply(&event));
            })
            .map_err(
                |e| warn!(error = ?e, conv_id = %conv_id, "Failed to apply buffered text to session"),
            )
            .ok();
        self.forward(event);
    }

    /// Hand an event to `StreamManager`, which turns it into the
    /// `StreamManagerEvent` the UI is subscribed to.
    fn forward(&mut self, event: SessionEvent) {
        let Some(sm) = self.stream_manager.clone() else {
            return;
        };
        let conv_id = self.conv_id.clone();
        sm.update(
            &mut self.cx,
            |sm: &mut crate::chatty::models::StreamManager, cx| {
                sm.handle_session_event(&conv_id, event, cx)
            },
        )
        .map_err(|e| warn!(error = ?e, "Failed to forward session event to StreamManager"))
        .ok();
    }

    /// Attach the turn's trace to the stream before it ends.
    fn set_trace(&mut self) {
        let trace_json = extract_trace_json(&self.chat_view, &self.conv_id, &mut self.cx);
        let Some(sm) = self.stream_manager.clone() else {
            return;
        };
        let conv_id = self.conv_id.clone();
        sm.update(
            &mut self.cx,
            |sm: &mut crate::chatty::models::StreamManager, _cx| {
                sm.set_trace(&conv_id, trace_json);
            },
        )
        .map_err(|e| warn!(error = ?e, "Failed to set trace on the stream"))
        .ok();
    }

    /// Protocol / loop-guard follow-up: sent after the turn is finalized so
    /// the UI shows the previous response first. Hidden from the transcript
    /// bubble list.
    fn inject_follow_up(&mut self, prompt: String) {
        let conv_id = self.conv_id.clone();
        debug!(conv_id = %conv_id, "Injecting protocol follow-up after stream");
        // A follow-up that never reaches the model looks exactly like a hung
        // model from the user's seat, so a failure here names the conversation
        // (AGE-151).
        self.weak_ctrl
            .update(&mut self.cx, |app, cx| {
                app.send_protocol_follow_up(prompt, cx);
            })
            .map_err(|e| {
                warn!(
                    error = ?e,
                    conv_id = %conv_id,
                    "Protocol follow-up dropped: the conversation will look stalled"
                )
            })
            .ok();
    }

    /// Push a changed todo snapshot into the plan strip and to disk. The
    /// conversation already carries it (`AgentSession::apply`).
    fn publish_todo_snapshot(&mut self, snapshot: AgentTaskSnapshot) {
        let conv_id = self.conv_id.clone();
        self.chat_view
            .update(&mut self.cx, |view, cx| {
                if view.conversation_id().map(|id| id.as_str()) == Some(conv_id.as_str()) {
                    view.set_agent_task_snapshot(snapshot, cx);
                }
            })
            .map_err(
                |e| warn!(error = ?e, "Failed to update agent todo panel after todo tool result"),
            )
            .ok();

        self.weak_ctrl
            .update(&mut self.cx, |app, cx| {
                app.persist_conversation(&conv_id, cx);
            })
            .map_err(|e| warn!(error = ?e, "Failed to persist agent todo panel snapshot to disk"))
            .ok();
    }

    /// If the follow-up budget ran out with verification still pending, show
    /// it in the plan UI rather than silently freezing on the last todo. The
    /// session records it on the conversation when it finishes the turn.
    fn publish_skipped_verification(&mut self) {
        let conv_id = self.conv_id.clone();
        let snapshot = self
            .cx
            .update_global::<ConversationsStore, _>(|store, _cx| {
                store
                    .get_conversation(&conv_id)
                    .and_then(|conv| conv.agent_task_snapshot().cloned())
                    .filter(|snapshot| snapshot.verification_skipped)
            })
            .map_err(|e| warn!(error = ?e, "Failed to read the skipped-verification snapshot"))
            .ok()
            .flatten();
        if let Some(snapshot) = snapshot {
            self.publish_todo_snapshot(snapshot);
        }
    }

    fn on_progress(&mut self, progress: InvokeAgentProgress) {
        let conv_id = self.conv_id.clone();
        match progress {
            InvokeAgentProgress::Started {
                agent_name,
                prompt,
                source,
            } => {
                let label = format!("[Agent: {}] {}", agent_name, prompt);
                self.chat_view
                    .update(&mut self.cx, |view, cx| {
                        if view.conversation_id().map(|id| id.as_str()) == Some(conv_id.as_str()) {
                            view.start_delegation_progress(&label, source, cx);
                        }
                    })
                    .map_err(|e| warn!(error = ?e, conv_id = %conv_id, "Failed to update chat view with sub-agent start"))
                    .ok();
            }
            InvokeAgentProgress::Text(text) => {
                self.chat_view
                    .update(&mut self.cx, |view, cx| {
                        if view.conversation_id().map(|id| id.as_str()) == Some(conv_id.as_str()) {
                            view.append_delegation_progress(&text, cx);
                        }
                    })
                    .map_err(|e| warn!(error = ?e, conv_id = %conv_id, "Failed to update chat view with sub-agent progress"))
                    .ok();
            }
            InvokeAgentProgress::Finished { success, result } => {
                self.chat_view
                    .update(&mut self.cx, |view, cx| {
                        if view.conversation_id().map(|id| id.as_str()) == Some(conv_id.as_str()) {
                            view.finalize_delegation_progress(success, result, cx);
                        }
                    })
                    .map_err(|e| warn!(error = ?e, conv_id = %conv_id, "Failed to update chat view with sub-agent finish"))
                    .ok();
            }
        }
    }
}

#[cfg(test)]
#[path = "session_characterization.rs"]
mod characterization;

/// True when `path`'s extension is `pdf`, checked case-insensitively so
/// `report.PDF` is recognized the same as `report.pdf` (finding F7, AGE-218).
pub(super) fn is_pdf_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("pdf"))
        .unwrap_or(false)
}

/// Select attachment paths from the immediately preceding assistant message
/// (the previous turn), if any, that the current model can handle. Returns
/// paths filtered by capability.
///
/// Stops at the most recent assistant entry regardless of whether it has
/// attachments — walking further back would re-attach an artifact from an
/// older turn on every later send (finding F2, AGE-216).
///
/// Used to include tool-generated images/PDFs in follow-up prompts so the
/// LLM can reference previously displayed files.
pub(super) fn select_recent_assistant_attachments(
    entries: &[chatty_core::models::MessageEntry],
    supports_images: bool,
    supports_pdf: bool,
) -> Vec<PathBuf> {
    if !supports_images && !supports_pdf {
        return Vec::new();
    }
    let Some(last_assistant_entry) = entries.iter().rev().find(|entry| {
        matches!(
            entry.message,
            rig_core::completion::Message::Assistant { .. }
        )
    }) else {
        return Vec::new();
    };
    last_assistant_entry
        .attachment_paths
        .iter()
        .filter(|path| {
            if is_pdf_path(path) {
                supports_pdf
            } else {
                supports_images
            }
        })
        .cloned()
        .collect()
}

/// Convert a file attachment to a rig-core UserContent
pub(super) async fn attachment_to_user_content(
    path: &Path,
) -> anyhow::Result<rig_core::message::UserContent> {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();

    let data = tokio::fs::read(path).await?;
    let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &data);

    match ext.as_str() {
        "png" => Ok(rig_core::message::UserContent::image_base64(
            b64,
            Some(rig_core::completion::message::ImageMediaType::PNG),
            Some(rig_core::completion::message::ImageDetail::Auto),
        )),
        "jpg" | "jpeg" => Ok(rig_core::message::UserContent::image_base64(
            b64,
            Some(rig_core::completion::message::ImageMediaType::JPEG),
            Some(rig_core::completion::message::ImageDetail::Auto),
        )),
        "gif" => Ok(rig_core::message::UserContent::image_base64(
            b64,
            Some(rig_core::completion::message::ImageMediaType::GIF),
            Some(rig_core::completion::message::ImageDetail::Auto),
        )),
        "webp" => Ok(rig_core::message::UserContent::image_base64(
            b64,
            Some(rig_core::completion::message::ImageMediaType::WEBP),
            Some(rig_core::completion::message::ImageDetail::Auto),
        )),
        "svg" => Ok(rig_core::message::UserContent::image_base64(
            b64,
            Some(rig_core::completion::message::ImageMediaType::SVG),
            Some(rig_core::completion::message::ImageDetail::Auto),
        )),
        "pdf" => Ok(rig_core::message::UserContent::Document(
            rig_core::completion::message::Document {
                data: rig_core::completion::message::DocumentSourceKind::Base64(b64),
                media_type: Some(rig_core::completion::message::DocumentMediaType::PDF),
                additional_params: None,
            },
        )),
        _ => Err(anyhow::anyhow!("Unsupported file type: {}", ext)),
    }
}

/// Serialize the current trace for `conv_id`, preferring the live ChatView and
/// falling back to the Conversation model when the user has switched away.
///
/// Both the normal and the errored stream paths need this: a turn that died
/// mid-flight still has tool calls worth keeping in the transcript.
fn extract_trace_json(
    chat_view: &gpui::Entity<crate::chatty::views::ChatView>,
    conv_id: &str,
    cx: &mut AsyncApp,
) -> Option<serde_json::Value> {
    let trace_from_view = chat_view
        .update(cx, |view, _cx| view.extract_current_trace())
        .map_err(|e| warn!(error = ?e, conv_id = %conv_id, "Failed to read trace from ChatView"))
        .ok()
        .flatten();

    let trace = trace_from_view.or_else(|| {
        cx.try_read_global::<ConversationsStore, _>(|store, _| {
            store
                .get_conversation(conv_id)
                .and_then(|conv| conv.streaming_trace().cloned())
        })
        .flatten()
    });

    trace.and_then(|trace| match serde_json::to_value(&trace) {
        Ok(val) => {
            debug!(conv_id = %conv_id, items = trace.items.len(), "Trace serialized successfully");
            Some(val)
        }
        Err(e) => {
            error!(conv_id = %conv_id, error = ?e, "Failed to serialize trace in run_llm_stream");
            None
        }
    })
}

#[cfg(test)]
mod tests {
    // Re-import standard #[test] to shadow gpui::test from `use gpui::*`
    use core::prelude::rust_2021::test;

    use std::cell::RefCell;
    use std::rc::Rc;

    use super::*;
    use chatty_core::models::MessageEntry;
    use rig_core::completion::message::{AssistantContent, Text};
    use rig_core::message::{Message, UserContent};

    fn user_msg(text: &str) -> Message {
        Message::User {
            content: vec![UserContent::text(text)],
        }
    }

    fn assistant_msg(text: &str) -> Message {
        Message::Assistant {
            id: None,
            content: vec![AssistantContent::Text(Text::new(text.to_string()))],
        }
    }

    fn entry(message: Message, attachments: Vec<PathBuf>) -> MessageEntry {
        MessageEntry {
            message,
            system_trace: None,
            attachment_paths: attachments,
            timestamp: None,
            feedback: None,
        }
    }

    #[test]
    fn select_attachments_no_assistant_messages() {
        let entries = vec![entry(user_msg("hello"), vec![])];
        let result = select_recent_assistant_attachments(&entries, true, true);
        assert!(result.is_empty());
    }

    // Auth-kind classification and its retry-once policy are now tested
    // once in chatty-core (llm_service::tests::classifies_401_as_auth /
    // classifies_403_as_auth, stream_processor::tests::auth_retries_once_then_stops,
    // AGE-244 / D5) instead of here per-provider: the desktop no longer knows
    // or cares which provider produced the error, and since the Entra token is
    // attached per request (AGE-245) it has nothing to refresh either — the
    // arm only warns.

    #[test]
    fn select_attachments_returns_image_paths() {
        let entries = vec![
            entry(user_msg("hi"), vec![]),
            entry(
                assistant_msg("here's a chart"),
                vec![PathBuf::from("/tmp/chart.png")],
            ),
        ];
        let result = select_recent_assistant_attachments(&entries, true, false);
        assert_eq!(result, vec![PathBuf::from("/tmp/chart.png")]);
    }

    #[test]
    fn select_attachments_filters_pdf_when_unsupported() {
        let entries = vec![
            entry(user_msg("hi"), vec![]),
            entry(
                assistant_msg("report"),
                vec![
                    PathBuf::from("/tmp/chart.png"),
                    PathBuf::from("/tmp/report.pdf"),
                ],
            ),
        ];
        // images supported, pdf not
        let result = select_recent_assistant_attachments(&entries, true, false);
        assert_eq!(result, vec![PathBuf::from("/tmp/chart.png")]);
    }

    #[test]
    fn select_attachments_filters_images_when_unsupported() {
        let entries = vec![
            entry(user_msg("hi"), vec![]),
            entry(
                assistant_msg("report"),
                vec![
                    PathBuf::from("/tmp/chart.png"),
                    PathBuf::from("/tmp/report.pdf"),
                ],
            ),
        ];
        // pdf supported, images not
        let result = select_recent_assistant_attachments(&entries, false, true);
        assert_eq!(result, vec![PathBuf::from("/tmp/report.pdf")]);
    }

    #[test]
    fn select_attachments_returns_most_recent_only() {
        let entries = vec![
            entry(user_msg("first"), vec![]),
            entry(
                assistant_msg("old chart"),
                vec![PathBuf::from("/tmp/old.png")],
            ),
            entry(user_msg("second"), vec![]),
            entry(
                assistant_msg("new chart"),
                vec![PathBuf::from("/tmp/new.png")],
            ),
        ];
        let result = select_recent_assistant_attachments(&entries, true, true);
        assert_eq!(result, vec![PathBuf::from("/tmp/new.png")]);
    }

    #[test]
    fn select_attachments_does_not_walk_back_past_the_last_assistant_turn() {
        // The immediately preceding assistant message has no attachments, so
        // nothing is attached — even though an earlier turn did produce one.
        // Walking further back would re-attach a stale artifact on every
        // later send (finding F2, AGE-216).
        let entries = vec![
            entry(user_msg("first"), vec![]),
            entry(
                assistant_msg("has chart"),
                vec![PathBuf::from("/tmp/old.png")],
            ),
            entry(user_msg("second"), vec![]),
            entry(assistant_msg("no chart"), vec![]),
        ];
        let result = select_recent_assistant_attachments(&entries, true, true);
        assert!(result.is_empty());
    }

    #[test]
    fn select_attachments_no_capability_returns_empty() {
        let entries = vec![
            entry(user_msg("hi"), vec![]),
            entry(
                assistant_msg("chart"),
                vec![PathBuf::from("/tmp/chart.png")],
            ),
        ];
        let result = select_recent_assistant_attachments(&entries, false, false);
        assert!(result.is_empty());
    }

    #[test]
    fn select_attachments_pdf_case_insensitive() {
        let entries = vec![
            entry(user_msg("hi"), vec![]),
            entry(
                assistant_msg("report"),
                vec![PathBuf::from("/tmp/report.PDF")],
            ),
        ];
        let result = select_recent_assistant_attachments(&entries, false, true);
        assert_eq!(result, vec![PathBuf::from("/tmp/report.PDF")]);
    }

    #[test]
    fn is_pdf_path_is_case_insensitive() {
        assert!(is_pdf_path(&PathBuf::from("/tmp/report.pdf")));
        assert!(is_pdf_path(&PathBuf::from("/tmp/report.PDF")));
        assert!(is_pdf_path(&PathBuf::from("/tmp/report.Pdf")));
        assert!(!is_pdf_path(&PathBuf::from("/tmp/report")));
        assert!(!is_pdf_path(&PathBuf::from("/tmp/chart.png")));
    }

    // -------------------------------------------------------------------
    // TextBatch (AGE-166): upstream coalescing of raw SessionEvent::Text
    // chunks, ahead of `update_global`/`StreamManager::handle_chunk`.
    // -------------------------------------------------------------------

    /// The very first chunk of a turn must flush immediately — batching must
    /// never delay time-to-first-token.
    #[test]
    fn text_batch_flushes_the_first_chunk_immediately() {
        let mut batch = TextBatch::new();
        assert_eq!(batch.push("Hello").as_deref(), Some("Hello"));
    }

    /// A flood of chunks arriving faster than `FLUSH_INTERVAL` apart must
    /// coalesce into a single pending buffer rather than flushing each one —
    /// this is the "one update_global + one handle_chunk per flush interval"
    /// requirement (AGE-166 remediation #2), at the unit level: `push`
    /// returning `None` means neither call happens for that chunk.
    #[test]
    fn text_batch_coalesces_a_flood_after_the_first_chunk() {
        let mut batch = TextBatch::new();
        assert!(batch.push("a").is_some(), "first chunk flushes immediately");
        for _ in 0..999 {
            assert_eq!(
                batch.push("x"),
                None,
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
        let mut batch = TextBatch::new();
        batch.push("first"); // consumes the immediate-first-chunk flush
        batch.pending.push_str("buffered");
        batch.last_flush =
            Instant::now() - (crate::chatty::models::stream_manager::FLUSH_INTERVAL * 2);

        assert_eq!(batch.push(" more").as_deref(), Some("buffered more"));
    }

    /// `take()` (used before any non-text event) drains whatever is
    /// buffered regardless of how much time has passed, and is a no-op when
    /// there's nothing pending — so flushing before a tool call never emits
    /// a spurious empty TextChunk.
    #[test]
    fn text_batch_take_drains_regardless_of_elapsed_time() {
        let mut batch = TextBatch::new();
        assert_eq!(batch.take(), None, "nothing buffered yet");

        batch.push("first");
        batch.pending.push_str("not yet flushed");
        assert_eq!(batch.take().as_deref(), Some("not yet flushed"));
        assert_eq!(batch.take(), None, "draining twice must not re-emit");
    }

    // -------------------------------------------------------------------
    // DesktopSink::handle (AGE-166): the line this whole issue turns on is
    // `self.flush_text()` before a non-text event is applied/forwarded.
    // `tool_start_never_precedes_its_buffered_text` (stream_manager.rs)
    // proves the UI-facing ordering through `StreamManager`, but that test
    // drives `StreamManager::handle_session_event` directly — it never
    // constructs a `DesktopSink`, so it can't see the conversation-model
    // side of this bug: without the flush, a tool call's `text_before`
    // (chatty_core::session::mod::note_tool_started, persisted into the
    // trace and read back for interleaved transcript rendering) would be
    // truncated by up to one `FLUSH_INTERVAL`, and the conversation's own
    // `streaming_message` would silently lose the buffered chunk. This test
    // builds a real `DesktopSink` — a real windowed `ChatView`, a real
    // `AgentSession`/`Conversation` (Ollama, so client construction is
    // network-free), and a real `ConversationsStore` global — and fails on
    // both fronts if `self.flush_text()` is removed from `handle`.
    // -------------------------------------------------------------------

    #[gpui::test]
    async fn desktop_sink_flushes_buffered_text_before_tool_call_starts(
        cx: &mut gpui::TestAppContext,
    ) {
        // `ChatView`'s first frame hard-reads six globals (CLAUDE.md
        // "Desktop boot order") plus gpui-component's own `Theme` global
        // (`gpui_component::init`, which `main.rs` calls before
        // `open_window` for the same reason) — a missing one panics on
        // first paint.
        cx.update(|cx| {
            gpui_component::init(cx);
            cx.set_global(crate::settings::models::general_model::GeneralSettingsModel::default());
            cx.set_global(crate::settings::models::ExecutionSettingsModel::default());
            cx.set_global(crate::settings::models::ExtensionsModel::default());
            cx.set_global(crate::chatty::models::ErrorStore::new(100));
            cx.set_global(crate::auto_updater::AutoUpdater::new("0.0.0"));
            cx.set_global(ConversationsStore::new());
        });

        // gpui-component requires the window's first layer to be its own
        // `Root` (`Root::update`/`Root::read` `.expect()` on that), so
        // `ChatView` can't be the literal window root the way `main.rs`
        // wraps `ChattyApp` in one — capture the `ChatView` entity out of
        // the closure instead of trying to type it back out of `Root`'s
        // type-erased `AnyView`.
        let chat_view_slot: Rc<RefCell<Option<Entity<ChatView>>>> = Rc::default();
        let slot_for_window = chat_view_slot.clone();
        // Bound (not `_`-discarded) so the window stays open for the rest
        // of the test rather than closing when the handle drops.
        let _window = cx.add_window(move |window, cx| {
            let view = cx.new(|cx| ChatView::new(window, cx));
            *slot_for_window.borrow_mut() = Some(view.clone());
            gpui_component::Root::new(view, window, cx)
        });
        let chat_view = chat_view_slot
            .borrow_mut()
            .take()
            .expect("ChatView entity should have been captured while opening the window");

        // A real session owning a real (network-free) conversation, exactly
        // as `desktop_send_path_matches_goldens` and chatty-core's own
        // `session_with_conversation` fixture build one.
        let _ = chatty_core::init_repositories();
        let conv_id = "conv-order".to_string();
        let mut session = AgentSession::new(AgentSessionConfig {
            execution_settings:
                chatty_core::settings::models::execution_settings::ExecutionSettingsModel::default(),
            surface: chatty_core::services::StreamSurface::Desktop,
            loop_guard: true,
        });
        let model_config = chatty_core::settings::models::models_store::ModelConfig::new(
            "m1".to_string(),
            "Test Model".to_string(),
            chatty_core::settings::models::providers_store::ProviderType::Ollama,
            "llama3.2".to_string(),
        );
        let provider_config = chatty_core::settings::models::providers_store::ProviderConfig::new(
            "Ollama".to_string(),
            chatty_core::settings::models::providers_store::ProviderType::Ollama,
        );
        session
            .create_conversation(
                conv_id.clone(),
                "Test".to_string(),
                &model_config,
                &provider_config,
                AgentBuildContext::from_services(AgentServices::default()),
            )
            .await
            .expect("conversation should build without network access (Ollama)");

        cx.update(|cx| {
            cx.update_global::<ConversationsStore, _>(|store, _cx| {
                store.insert_loaded(session);
            });
        });

        let mut sink = DesktopSink {
            conv_id: conv_id.clone(),
            cx: cx.to_async(),
            chat_view,
            stream_manager: None,
            weak_ctrl: gpui::WeakEntity::new_invalid(),
            text_batch: TextBatch::new(),
        };

        // First chunk flushes immediately (should_flush_text's first-chunk
        // rule); the second arrives well within FLUSH_INTERVAL and stays
        // buffered in `sink.text_batch` — exactly the case that needs
        // `flush_text()` before the tool call below.
        sink.handle(SessionEvent::Text("Let me".to_string()));
        sink.handle(SessionEvent::Text(" check that.".to_string()));
        sink.handle(SessionEvent::ToolCallStarted {
            id: "call-1".to_string(),
            name: "read_file".to_string(),
        });

        cx.update(|cx| {
            let store = cx.global::<ConversationsStore>();
            let conv = store
                .get_conversation(&conv_id)
                .expect("conversation is loaded");

            assert_eq!(
                conv.streaming_message().map(String::as_str),
                Some("Let me check that."),
                "the buffered second chunk must be applied to the conversation before \
                 the tool call — without DesktopSink::handle's flush, it stays stranded \
                 in the sink's own buffer and never reaches Conversation.streaming_message"
            );

            let trace = conv
                .streaming_trace()
                .expect("the tool call should have opened a streaming trace");
            let tool_call = trace
                .items
                .iter()
                .find_map(|item| match item {
                    chatty_core::models::message_types::TraceItem::ToolCall(tc) => Some(tc),
                    #[allow(unreachable_patterns)]
                    _ => None,
                })
                .expect("a tool call block was recorded");
            assert_eq!(
                tool_call.text_before, "Let me check that.",
                "note_tool_started reads Conversation.streaming_message for text_before \
                 (chatty_core/src/session/mod.rs) — if DesktopSink::handle's flush is \
                 removed, this is truncated to just the first chunk"
            );
        });
    }
}
