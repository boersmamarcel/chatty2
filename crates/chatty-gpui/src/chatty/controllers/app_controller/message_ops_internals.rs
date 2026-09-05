//! Internal helpers extracted from `message_ops.rs` to keep that file
//! under ~1300 LOC. All items are `pub(super)` and only used by
//! `message_ops.rs` siblings of this file.
//!
//! See `message_ops.rs` for the high-level `ChattyApp` methods that
//! orchestrate these helpers.

use super::*;
// The desktop drives chatty-core's stream loop, same as chatty-tui: the loop,
// its cancellation checks and its stall watchdog live there, and only the
// dispatch below is desktop-specific (AGE-192).
use chatty_core::services::ChunkAction;
use chatty_core::tools::invoke_agent_tool::InvokeAgentProgress;

/// Parameters for the shared LLM stream processing.
pub(super) struct LlmStreamParams {
    pub(super) conv_id: String,
    pub(super) agent: AgentClient,
    pub(super) history: Vec<rig_core::completion::Message>,
    pub(super) user_contents: Vec<rig_core::message::UserContent>,
    pub(super) add_user_message_to_model: bool,
    /// True when a human turn starts this stream. Injected protocol follow-ups
    /// pass `false` so they keep the todo state of the turn they belong to.
    pub(super) reset_agent_task: bool,
    pub(super) attachment_paths: Vec<PathBuf>,
    pub(super) provider_type: chatty_core::settings::models::providers_store::ProviderType,
    pub(super) chat_view: Entity<ChatView>,
    pub(super) stream_manager: Option<Entity<crate::chatty::models::StreamManager>>,
    pub(super) cancel_flag: Arc<AtomicBool>,
    pub(super) invoke_agent_progress_slot:
        chatty_core::tools::invoke_agent_tool::InvokeAgentProgressSlot,
    /// Weak controller handle — used to inject follow-up messages when
    /// AgentLoopGuard detects a loop or deadline.
    pub(super) weak_ctrl: gpui::WeakEntity<ChattyApp>,
}

/// Maps [`StreamChunk`]s and sub-agent progress onto the desktop's UI state.
///
/// The desktop's half of the seam `chatty-tui` has used since AGE-188: the loop
/// itself lives in `chatty_core::services::run_stream_loop`, and this type is
/// only the dispatch. It holds its own [`AsyncApp`], because the trait's methods
/// take `&mut self` rather than a context — which is also why nothing here may
/// be `Send`.
///
/// Every arm writes to two places, in this order: the `Conversation` model,
/// which is the source of truth for a stream whose conversation is not on
/// screen, and then `StreamManager`, which emits the event the UI subscribes to.
///
/// Finalization is deliberately *not* here. `run_llm_stream` still owns trace
/// extraction, `finalize_stream` and follow-up injection, and reads
/// [`Self::pending_follow_up`] and [`Self::stream_errored`] back off the handler
/// once the loop returns.
pub(super) struct GpuiStreamHandler {
    conv_id: String,
    cx: AsyncApp,
    chat_view: Entity<ChatView>,
    stream_manager: Option<Entity<crate::chatty::models::StreamManager>>,
    weak_ctrl: gpui::WeakEntity<ChattyApp>,
    provider_type: chatty_core::settings::models::providers_store::ProviderType,
    agent_task_controller: chatty_core::services::AgentTaskController,
    loop_guard: chatty_core::services::AgentLoopGuard,
    cancel_flag: Arc<AtomicBool>,
    /// id → name and id → args for the tool call currently in flight.
    pending_tool_name: std::collections::HashMap<String, String>,
    pending_tool_args: std::collections::HashMap<String, String>,
    /// Injected by `run_llm_stream` after the loop; see the type docs.
    pub(super) pending_follow_up: Option<String>,
    /// Set when the stream ended via `StreamChunk::Error` or a transport `Err`,
    /// which already emitted `StreamEnded` and dropped the stream from the
    /// manager. `run_llm_stream` skips its own finalize when this is set.
    pub(super) stream_errored: bool,
    text_overflow_stop_requested: bool,
}

impl GpuiStreamHandler {
    /// Forward a chunk to `StreamManager`, which is what turns it into a
    /// `StreamManagerEvent` the UI is subscribed to.
    fn forward(&mut self, chunk: StreamChunk) {
        let Some(sm) = self.stream_manager.clone() else {
            return;
        };
        let conv_id = self.conv_id.clone();
        sm.update(
            &mut self.cx,
            |sm: &mut crate::chatty::models::StreamManager, cx| {
                sm.handle_chunk(&conv_id, chunk, cx)
            },
        )
        .map_err(|e| warn!(error = ?e, "Failed to forward chunk to StreamManager"))
        .ok();
    }

    /// Attach the turn's trace before a terminal error drops the stream.
    ///
    /// `handle_chunk`'s Error arm removes the stream from the manager, so the
    /// trace has to be set first or the failed turn loses its tool calls.
    fn capture_trace_before_error(&mut self) {
        let trace_json = extract_trace_json(&self.chat_view, &self.conv_id, &mut self.cx);
        self.stream_errored = true;
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
        .map_err(|e| warn!(error = ?e, "Failed to set trace before error"))
        .ok();
    }

    /// Push the agent's todo snapshot into the conversation, the plan strip and
    /// disk, after a tool that can have changed it.
    fn publish_todo_snapshot(&mut self) {
        let snapshot = self.agent_task_controller.snapshot();
        let conv_id = self.conv_id.clone();

        self.cx
            .update_global::<ConversationsStore, _>(|store, _cx| {
                if let Some(conv) = store.get_conversation_mut(&conv_id) {
                    conv.set_agent_task_snapshot(Some(snapshot.clone()));
                }
            })
            .map_err(|e| {
                warn!(error = ?e, "Failed to persist agent todo panel snapshot in conversation state")
            })
            .ok();

        let snapshot_for_view = snapshot.clone();
        self.chat_view
            .update(&mut self.cx, |view, cx| {
                if view.conversation_id().map(|id| id.as_str()) == Some(conv_id.as_str()) {
                    view.set_agent_task_snapshot(snapshot_for_view, cx);
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
}

impl chatty_core::services::StreamChunkHandler for GpuiStreamHandler {
    fn on_stream_started(&mut self) {
        debug!(conv_id = %self.conv_id, "Entering stream processing loop");
    }

    async fn on_chunk(
        &mut self,
        chunk_result: anyhow::Result<StreamChunk>,
    ) -> anyhow::Result<ChunkAction> {
        // PHASE 1: local state that has to change before the chunk is forwarded.
        match chunk_result {
            Ok(StreamChunk::Text(ref text)) => {
                let conv_id = self.conv_id.clone();
                self.cx
                    .update_global::<ConversationsStore, _>(|store, _cx| {
                        if let Some(conv) = store.get_conversation_mut(&conv_id) {
                            conv.append_streaming_content(text);
                        }
                    })
                    .map_err(
                        |e| warn!(error = ?e, "Failed to update conversation streaming content"),
                    )
                    .ok();
                // Verbosity guard: flag if the model is writing a wall of text
                // with no tools.
                if !self.text_overflow_stop_requested && self.loop_guard.on_text_chunk(text.len()) {
                    self.text_overflow_stop_requested = true;
                    debug!(conv_id = %self.conv_id,
                        "Text-only response exceeded verbosity limit; will inject brevity prompt after response completes.");
                }
            }
            Ok(StreamChunk::TokenUsage { .. }) => {
                // Token usage tracked by StreamManager
            }
            Ok(StreamChunk::Done) => {
                debug!(conv_id = %self.conv_id, "Received Done chunk");
                // If the model produced too much text without a tool call, queue
                // a brevity prompt.
                if self.text_overflow_stop_requested && self.pending_follow_up.is_none() {
                    self.pending_follow_up = Some(
                        "You produced a long response without any tool call. \
                         If you have enough information, give your final answer now. \
                         Otherwise, make a single focused tool call to get what you need."
                            .to_string(),
                    );
                }
                if self.pending_follow_up.is_none() {
                    self.pending_follow_up = self.agent_task_controller.stream_end_follow_up();
                }
                // Forward before breaking, so the UI sees the turn close.
                self.forward(StreamChunk::Done);
                return Ok(ChunkAction::Break);
            }
            Ok(StreamChunk::Error(ref err)) => {
                error!(error = %err, conv_id = %self.conv_id, "Stream error");

                // Detect authentication errors (401/Unauthorized)
                if should_refresh_azure_auth(&self.provider_type, err) {
                    tracing::warn!("Detected Azure auth error - token likely expired");
                    if let Some(cache) = self
                        .cx
                        .update(|cx| {
                            cx.try_global::<chatty_core::auth::AzureTokenCache>()
                                .cloned()
                        })
                        .map_err(|e| warn!(error = ?e, "Failed to read Azure token cache global"))
                        .ok()
                        .flatten()
                    {
                        if let Err(e) = cache.refresh_token().await {
                            error!(error = ?e, "Failed to refresh Azure token after 401 error");
                        } else {
                            tracing::info!("Azure token refreshed successfully.");
                        }
                    }
                } else if matches!(
                    self.provider_type,
                    chatty_core::settings::models::providers_store::ProviderType::OpenRouter
                ) && is_auth_stream_error(err)
                {
                    tracing::warn!(
                        "Detected OpenRouter authentication error - check the configured API key/header"
                    );
                }
            }
            Ok(StreamChunk::ToolCallStarted { ref id, ref name }) => {
                self.pending_tool_name.insert(id.clone(), name.clone());
            }
            Ok(StreamChunk::ToolCallInput {
                ref id,
                ref arguments,
            }) => {
                self.pending_tool_args.insert(id.clone(), arguments.clone());
            }
            Ok(StreamChunk::ToolCallResult { ref id, .. }) => {
                let tool_name = self.pending_tool_name.remove(id).unwrap_or_default();
                let tool_args = self.pending_tool_args.remove(id).unwrap_or_default();
                if is_agent_todo_tool(&tool_name) {
                    self.publish_todo_snapshot();
                }
                if self.pending_follow_up.is_none()
                    && let Some(prompt) = self.agent_task_controller.observe_tool_result(&tool_name)
                {
                    debug!(
                        conv_id = %self.conv_id,
                        "Agent todo protocol: multiple tool results observed before write_todos"
                    );
                    if follow_up_requires_cancel(FollowUpReason::TodoProtocol) {
                        self.cancel_flag.store(true, Ordering::Relaxed);
                    }
                    self.pending_follow_up = Some(prompt);
                }
                if let Some(pivot) = self.loop_guard.on_tool_completed(&tool_name, &tool_args) {
                    debug!(conv_id = %self.conv_id, pivot = %pivot,
                        "AgentLoopGuard loop detected; cancelling stream");
                    if follow_up_requires_cancel(FollowUpReason::LoopGuard) {
                        self.cancel_flag.store(true, Ordering::Relaxed);
                    }
                    self.pending_follow_up = Some(pivot);
                }
            }
            Ok(_) => {
                // ApprovalRequested, ApprovalResolved, ClarificationRequested,
                // ToolCallError: no local state
            }
            Err(ref e) => {
                error!(error = %e, conv_id = %self.conv_id, "Stream error");
            }
        }

        // PHASE 2: forward every chunk, so the UI's subscription sees it.
        match chunk_result {
            Ok(chunk) => {
                let is_break = matches!(chunk, StreamChunk::Error(_));
                if is_break {
                    // Same terminal path as the Err arm below: the manager drops
                    // the stream, so keep the trace and skip the redundant
                    // finalize afterwards.
                    self.capture_trace_before_error();
                }
                self.forward(chunk);
                if is_break {
                    Ok(ChunkAction::Break)
                } else {
                    Ok(ChunkAction::Continue)
                }
            }
            Err(e) => {
                let message = e.to_string();

                // A truncated tool call is a model defect, not a dead
                // connection: hand the parse error back and let it retry.
                //
                // The cap has to live in conversation history, not in a field:
                // the handler is rebuilt for every injected follow-up, so a flag
                // here would reset each time and the retry would never
                // terminate. That is AGE-150 Defect 2, and it is easy to rebuild
                // by accident.
                if is_malformed_tool_call_error(&message)
                    && self.pending_follow_up.is_none()
                    && !already_asked_to_retry(&self.conv_id, &mut self.cx)
                {
                    warn!(conv_id = %self.conv_id, error = %message,
                        "Malformed tool-call JSON; asking the model to retry");
                    self.pending_follow_up = Some(MALFORMED_TOOL_CALL_FOLLOW_UP.to_string());
                }

                // Keep the failed turn's tool calls in the transcript.
                self.capture_trace_before_error();
                self.forward(StreamChunk::Error(message));
                Ok(ChunkAction::Break)
            }
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
                let label_for_store = label.clone();
                let source_for_store = source.clone();
                self.cx
                    .update_global::<ConversationsStore, _>(|store, _cx| {
                        if let Some(conv) = store.get_conversation_mut(&conv_id) {
                            conv.start_sub_agent_progress(&label_for_store, source_for_store);
                        }
                    })
                    .map_err(|e| warn!(error = ?e, conv_id = %conv_id, "Failed to persist sub-agent start"))
                    .ok();
                self.chat_view
                    .update(&mut self.cx, |view, cx| {
                        if view.conversation_id().map(|id| id.as_str()) == Some(conv_id.as_str()) {
                            view.start_sub_agent_progress(&label, source, cx);
                        }
                    })
                    .map_err(|e| warn!(error = ?e, conv_id = %conv_id, "Failed to update chat view with sub-agent start"))
                    .ok();
            }
            InvokeAgentProgress::Text(text) => {
                let text_for_store = text.clone();
                self.cx
                    .update_global::<ConversationsStore, _>(|store, _cx| {
                        if let Some(conv) = store.get_conversation_mut(&conv_id) {
                            conv.append_sub_agent_progress(&text_for_store);
                        }
                    })
                    .map_err(|e| warn!(error = ?e, conv_id = %conv_id, "Failed to persist sub-agent progress"))
                    .ok();
                self.chat_view
                    .update(&mut self.cx, |view, cx| {
                        if view.conversation_id().map(|id| id.as_str()) == Some(conv_id.as_str()) {
                            view.append_sub_agent_progress(&text, cx);
                        }
                    })
                    .map_err(|e| warn!(error = ?e, conv_id = %conv_id, "Failed to update chat view with sub-agent progress"))
                    .ok();
            }
            InvokeAgentProgress::Finished { success, result } => {
                let result_for_store = result.clone();
                self.cx
                    .update_global::<ConversationsStore, _>(|store, _cx| {
                        if let Some(conv) = store.get_conversation_mut(&conv_id) {
                            conv.finalize_sub_agent_progress(success, result_for_store);
                        }
                    })
                    .map_err(|e| warn!(error = ?e, conv_id = %conv_id, "Failed to persist sub-agent final state"))
                    .ok();
                self.chat_view
                    .update(&mut self.cx, |view, cx| {
                        if view.conversation_id().map(|id| id.as_str()) == Some(conv_id.as_str()) {
                            view.finalize_sub_agent_progress(success, result, cx);
                        }
                    })
                    .map_err(|e| warn!(error = ?e, conv_id = %conv_id, "Failed to update chat view with sub-agent finish"))
                    .ok();
            }
        }
    }

    fn on_cancelled(&mut self) {
        debug!(conv_id = %self.conv_id, "Stream cancelled via cancellation token");
    }

    fn on_stream_ended(&mut self) {
        debug!(conv_id = %self.conv_id, "Stream loop finished, finalizing via StreamManager");
    }
}

#[cfg(test)]
#[path = "stream_handler_characterization.rs"]
mod characterization;

/// Shared LLM stream processing used by both `send_message` and `handle_regeneration`.
///
/// Handles:
/// 1. Approval channel setup
/// 2. `stream_prompt()` call
/// 3. Optionally adding user message to conversation model
/// 4. Stream processing loop (chunks -> ConversationsStore + StreamManager)
/// 5. Trace extraction and StreamManager finalization
///
/// Callers are responsible for their own preamble (conversation creation, UI message
/// addition, DPO recording, etc.) and for registering the returned task with StreamManager.
pub(super) async fn run_llm_stream(
    params: LlmStreamParams,
    cx: &mut AsyncApp,
) -> anyhow::Result<()> {
    let LlmStreamParams {
        conv_id,
        agent,
        history,
        user_contents,
        add_user_message_to_model,
        reset_agent_task,
        attachment_paths,
        provider_type,
        chat_view,
        stream_manager,
        cancel_flag,
        invoke_agent_progress_slot,
        weak_ctrl,
    } = params;
    // 1. Create approval notification channels
    let (approval_tx, approval_rx) = tokio::sync::mpsc::unbounded_channel();
    let (resolution_tx, resolution_rx) = tokio::sync::mpsc::unbounded_channel();

    crate::chatty::models::execution_approval_store::set_global_approval_notifier(
        approval_tx.clone(),
    );
    cx.update_global::<crate::chatty::models::execution_approval_store::ExecutionApprovalStore, _>(
        |store, _cx| {
            store.set_notifiers(approval_tx, resolution_tx);
        },
    )
    .map_err(|e| warn!(error = ?e, "Failed to update approval store with notifiers"))
    .ok();

    let (clarification_tx, clarification_rx) = tokio::sync::mpsc::unbounded_channel();
    chatty_core::models::clarification_store::set_global_clarification_notifier(clarification_tx);

    // 2. Get max agent turns and workspace dir
    let max_agent_turns = cx
        .update(|cx| cx.global::<ExecutionSettingsModel>().max_agent_turns as usize)
        .unwrap_or(10);
    // Use per-conversation workspace dir override if set, fall back to global setting
    let _workspace_dir = cx
        .update(|cx| {
            // Check per-conversation override first
            let per_conv = cx
                .global::<ConversationsStore>()
                .get_conversation(&conv_id)
                .and_then(|c| {
                    c.working_dir()
                        .map(|p| normalize_workspace_path(p).to_string_lossy().to_string())
                });
            // Fall back to global workspace_dir
            per_conv.or_else(|| {
                cx.global::<ExecutionSettingsModel>()
                    .workspace_dir
                    .as_deref()
                    .map(normalize_workspace_string)
            })
        })
        .map_err(|e| warn!(error = ?e, "Failed to resolve workspace directory override"))
        .ok()
        .flatten();

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
        let history_for_budget = history.clone();
        let conv_id_for_budget = conv_id.clone();

        let budget_inputs = cx
            .update(|cx| {
                gather_snapshot_inputs(
                    &conv_id_for_budget,
                    user_message_text_for_budget,
                    history_for_budget,
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

    // 3. Apply context shaping to keep history within LLM context limits.
    let shaped_history = {
        let settings = chatty_core::services::ContextShaperSettings::default();
        let shaped = chatty_core::services::shape_context(history, &settings, None).await;
        if let Some(stage) = shaped.stage_applied {
            debug!(conv_id = %conv_id, stage = ?stage, freed = shaped.chars_freed,
                "Context shaper applied");
        }
        shaped.messages
    };

    // 3b. Call stream_prompt with user contents directly (no auto-context injection)
    let agent_task_controller = agent.task_controller();
    // A new human turn starts from a clean todo protocol state: the controller
    // lives on the conversation's agent, so leftover state would otherwise nudge
    // forever and block a second write_todos (AGE-150).
    if reset_agent_task {
        agent_task_controller.reset();
    }
    let llm_user_contents = user_contents.clone();
    debug!(conv_id = %conv_id, "Calling stream_prompt()");
    let (mut stream, _user_message) = stream_prompt(
        &agent,
        &shaped_history,
        llm_user_contents,
        Some(approval_rx),
        Some(resolution_rx),
        Some(clarification_rx),
        max_agent_turns,
    )
    .await?;

    // 4. Optionally add user message to conversation model.
    if add_user_message_to_model {
        let user_message = rig_core::completion::Message::User {
            content: user_contents,
        };
        cx.update_global::<ConversationsStore, _>(|store, _cx| {
            if let Some(conv) = store.get_conversation_mut(&conv_id) {
                conv.add_user_message_with_attachments(user_message, attachment_paths);
            }
        })
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    }

    // 5. Install invoke_agent progress channel
    let mut progress_rx =
        chatty_core::services::install_progress_channel(&invoke_agent_progress_slot);

    // 5b. Drive the shared stream loop.
    //
    // The dispatch lives in `GpuiStreamHandler` above; the loop, its
    // cancellation checks and its stall watchdog are `chatty-core`'s, the same
    // ones chatty-tui runs. Behaviour that belongs to a turn is changed there
    // now, once, rather than twice (AGE-192).
    //
    // AgentLoopGuard detects repeated tool calls and verbosity bursts. Desktop
    // streams don't require an answer file, so answer_file_required=false.
    let mut handler = GpuiStreamHandler {
        conv_id: conv_id.clone(),
        cx: cx.clone(),
        chat_view: chat_view.clone(),
        stream_manager: stream_manager.clone(),
        weak_ctrl: weak_ctrl.clone(),
        provider_type,
        agent_task_controller: agent_task_controller.clone(),
        loop_guard: chatty_core::services::AgentLoopGuard::new(max_agent_turns, false),
        cancel_flag: cancel_flag.clone(),
        pending_tool_name: std::collections::HashMap::new(),
        pending_tool_args: std::collections::HashMap::new(),
        pending_follow_up: None,
        stream_errored: false,
        text_overflow_stop_requested: false,
    };

    chatty_core::services::run_stream_loop(
        &mut stream,
        &mut progress_rx,
        &cancel_flag,
        &mut handler,
    )
    .await?;

    // The loop drains any progress the stream outran before it returns, so the
    // sub-agent row is never left on a stale line.
    let GpuiStreamHandler {
        pending_follow_up,
        stream_errored,
        ..
    } = handler;

    // Clear the progress slot sender so stale references don't accumulate
    {
        let mut slot = invoke_agent_progress_slot.lock();
        *slot = None;
    }

    // 6. Extract trace and finalize via StreamManager
    debug!(conv_id = %conv_id, "Stream loop finished, finalizing via StreamManager");

    // A stream that ended in error already emitted StreamEnded and removed
    // itself from the manager (see `handle_chunk`'s Error arm), and its trace
    // was attached there. Calling finalize_stream again would only log
    // "no stream found" and drop the trace we just built.
    if !stream_errored {
        let trace_json = extract_trace_json(&chat_view, &conv_id, cx);

        if let Some(ref sm) = stream_manager {
            sm.update(cx, |sm: &mut crate::chatty::models::StreamManager, cx| {
                sm.set_trace(&conv_id, trace_json);
                sm.finalize_stream(&conv_id, cx);
            })
            .map_err(|e| warn!(error = ?e, "Failed to finalize stream in StreamManager"))
            .ok();
        }
    }

    // 6b. If the follow-up budget ran out with verification still pending, push
    // the snapshot so the To-dos card can say verification was skipped instead
    // of silently freezing on the last todo.
    let final_task_snapshot = agent_task_controller.snapshot();
    if final_task_snapshot.verification_skipped {
        let snapshot = final_task_snapshot;
        cx.update_global::<ConversationsStore, _>(|store, _cx| {
            if let Some(conv) = store.get_conversation_mut(&conv_id) {
                conv.set_agent_task_snapshot(Some(snapshot.clone()));
            }
        })
        .map_err(|e| warn!(error = ?e, "Failed to persist skipped-verification snapshot"))
        .ok();
        chat_view
            .update(cx, |view, cx| {
                if view.conversation_id().map(|id| id.as_str()) == Some(conv_id.as_str()) {
                    view.set_agent_task_snapshot(snapshot, cx);
                }
            })
            .map_err(|e| warn!(error = ?e, "Failed to show skipped verification in plan UI"))
            .ok();
        weak_ctrl
            .update(&mut *cx, |app, cx| {
                app.persist_conversation(&conv_id, cx);
            })
            .map_err(
                |e| warn!(error = ?e, "Failed to persist conversation after skipped verification"),
            )
            .ok();
    }

    // 7. Protocol / loop-guard follow-up: inject after finalization so the UI
    // shows the previous response first. Hidden from the transcript bubble list.
    if let Some(follow_up) = pending_follow_up {
        debug!(conv_id = %conv_id, "Injecting protocol follow-up after stream");
        // A follow-up that never reaches the model looks exactly like a hung
        // model from the user's seat, so a failure here names the conversation
        // (AGE-151).
        weak_ctrl
            .update(&mut *cx, |app, cx| {
                app.send_protocol_follow_up(follow_up, cx);
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

    Ok(())
}

pub(super) fn is_auth_stream_error(err: &str) -> bool {
    err.contains("401") || err.contains("Unauthorized")
}

pub(super) fn should_refresh_azure_auth(
    provider_type: &chatty_core::settings::models::providers_store::ProviderType,
    err: &str,
) -> bool {
    matches!(
        provider_type,
        chatty_core::settings::models::providers_store::ProviderType::AzureOpenAI
    ) && is_auth_stream_error(err)
}

fn is_agent_todo_tool(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "write_todos" | "update_todo" | "verify_completion"
    )
}

/// Select attachment paths from the most recent assistant message that the
/// current model can handle. Returns paths filtered by capability.
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
    for entry in entries.iter().rev() {
        if matches!(
            entry.message,
            rig_core::completion::Message::Assistant { .. }
        ) && !entry.attachment_paths.is_empty()
        {
            return entry
                .attachment_paths
                .iter()
                .filter(|path| {
                    let is_pdf = path
                        .extension()
                        .and_then(|e| e.to_str())
                        .map(|e| e.eq_ignore_ascii_case("pdf"))
                        .unwrap_or(false);
                    if is_pdf {
                        supports_pdf
                    } else {
                        supports_images
                    }
                })
                .cloned()
                .collect();
        }
    }
    Vec::new()
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

#[cfg(test)]
mod tests {
    // Re-import standard #[test] to shadow gpui::test from `use gpui::*`
    use core::prelude::rust_2021::test;

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

    #[test]
    fn azure_refresh_detection_is_provider_specific() {
        use chatty_core::settings::models::providers_store::ProviderType;

        let err = "ProviderError: Invalid status code 401 Unauthorized";
        assert!(should_refresh_azure_auth(&ProviderType::AzureOpenAI, err));
        assert!(!should_refresh_azure_auth(&ProviderType::OpenRouter, err));
        assert!(!should_refresh_azure_auth(&ProviderType::Ollama, err));
    }

    #[test]
    fn auth_stream_error_detects_common_401_text() {
        assert!(is_auth_stream_error(
            "Invalid status code 401 Unauthorized with message: missing auth"
        ));
        assert!(is_auth_stream_error("ProviderError: Unauthorized"));
        assert!(!is_auth_stream_error("ProviderError: rate limited"));
    }

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
    fn select_attachments_skips_assistant_without_attachments() {
        // Most recent assistant has no attachments, but an earlier one does
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
        // Should skip the empty one and find the older one
        assert_eq!(result, vec![PathBuf::from("/tmp/old.png")]);
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

/// Why a follow-up prompt is being queued for after the current turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FollowUpReason {
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
pub(super) fn follow_up_requires_cancel(reason: FollowUpReason) -> bool {
    match reason {
        FollowUpReason::TodoProtocol => false,
        FollowUpReason::LoopGuard => true,
    }
}

/// Injected once when a provider rejects a tool call for malformed JSON.
///
/// The `Agent protocol follow-up:` prefix is what
/// `chatty_core::services::is_protocol_follow_up_text` matches on, which keeps
/// this hidden from the transcript like every other injected nudge, and is what
/// [`already_asked_to_retry`] looks for in history.
pub(super) const MALFORMED_TOOL_CALL_FOLLOW_UP: &str = "Agent protocol follow-up: your last tool call was rejected because its JSON arguments \
     were malformed or truncated. Make the same call again, keeping the arguments small and \
     fully closed. If the arguments were large, write the content to a file in smaller steps \
     instead.";

/// Whether we already asked this conversation to retry a malformed tool call.
///
/// Bounds the retry to one attempt. Protocol follow-ups are appended to
/// conversation history even though they are filtered from the transcript, so
/// the last user message is a reliable place to look.
fn already_asked_to_retry(conv_id: &str, cx: &mut AsyncApp) -> bool {
    cx.try_read_global::<ConversationsStore, _>(|store, _| {
        let Some(conv) = store.get_conversation(conv_id) else {
            return false;
        };
        conv.entries()
            .iter()
            .rev()
            .find_map(|entry| match &entry.message {
                rig_core::message::Message::User { content } => {
                    Some(chatty_core::services::extract_user_text(content))
                }
                _ => None,
            })
            .is_some_and(|text| text.trim_start().starts_with(MALFORMED_TOOL_CALL_FOLLOW_UP))
    })
    .unwrap_or(false)
}

/// Whether a stream error is the provider handing us a tool call whose JSON
/// arguments were truncated or otherwise unparseable.
///
/// This is a model output defect, not a transport failure: the right response
/// is to tell the model what broke and let it retry, rather than ending the
/// conversation on a dead stream.
fn is_malformed_tool_call_error(error: &str) -> bool {
    let error = error.to_lowercase();
    error.contains("malformed json input")
        || (error.contains("tool call") && error.contains("malformed"))
}

#[cfg(test)]
mod stream_error_tests {
    use super::{
        FollowUpReason, MALFORMED_TOOL_CALL_FOLLOW_UP, follow_up_requires_cancel,
        is_malformed_tool_call_error,
    };

    /// The retry is bounded by spotting this text in history, and hidden from
    /// the transcript by the same prefix. Both depend on chatty-core's matcher
    /// recognising it — if the text drifts, the retry silently becomes
    /// unbounded and visible at once.
    #[test]
    fn retry_follow_up_is_recognised_as_a_protocol_nudge() {
        assert!(
            chatty_core::services::agent_task_controller::is_protocol_follow_up_text(
                MALFORMED_TOOL_CALL_FOLLOW_UP
            ),
            "the retry nudge must be filtered from the transcript like the others"
        );
    }

    #[test]
    fn retry_follow_up_explains_what_to_do_differently() {
        let text = MALFORMED_TOOL_CALL_FOLLOW_UP;
        assert!(text.contains("malformed or truncated"), "names the cause");
        assert!(text.contains("smaller steps"), "offers a way out");
    }

    #[test]
    fn detects_truncated_tool_call_arguments() {
        assert!(is_malformed_tool_call_error(
            "CompletionError: ResponseError: tool call `shell_execute` arrived with \
             malformed JSON input: EOF while parsing a string at line 1 column 308"
        ));
    }

    #[test]
    fn ignores_transport_and_auth_failures() {
        for other in [
            "CompletionError: ProviderError: Http client error: error decoding response body",
            "401 Unauthorized",
            "SSE error: connection reset",
        ] {
            assert!(
                !is_malformed_tool_call_error(other),
                "{other} should not be treated as a malformed tool call"
            );
        }
    }

    // -------------------------------------------------------------------
    // Follow-up cancellation policy (AGE-151)
    // -------------------------------------------------------------------

    /// The regression: the todo-protocol nudge cancelled the stream, which
    /// broke the loop before `Done` and discarded the turn's streamed text.
    #[test]
    fn todo_protocol_nudge_does_not_cancel_the_turn() {
        assert!(!follow_up_requires_cancel(FollowUpReason::TodoProtocol));
    }

    /// The loop guard still cancels: stopping the runaway turn is the point.
    #[test]
    fn loop_guard_pivot_still_cancels_the_turn() {
        assert!(follow_up_requires_cancel(FollowUpReason::LoopGuard));
    }
}
