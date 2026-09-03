//! Internal helpers extracted from `message_ops.rs` to keep that file
//! under ~1300 LOC. All items are `pub(super)` and only used by
//! `message_ops.rs` siblings of this file.
//!
//! See `message_ops.rs` for the high-level `ChattyApp` methods that
//! orchestrate these helpers.

#![allow(clippy::too_many_arguments)]

use super::*;

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

    // 5b. AgentLoopGuard: detects repeated tool calls (loops) and verbosity bursts.
    // Desktop streams don't require an answer file, so answer_file_required=false.
    let mut loop_guard = chatty_core::services::AgentLoopGuard::new(max_agent_turns, false);
    // Track id→name and id→args for the current tool call in flight.
    let mut pending_tool_name: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    let mut pending_tool_args: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    // If loop detection fires, we cancel the stream and inject this follow-up.
    let mut pending_follow_up: Option<String> = None;
    let mut text_overflow_stop_requested = false;
    // Set when the stream terminated via StreamChunk::Error, which already
    // emitted StreamEnded and removed the stream from the manager.
    let mut stream_errored = false;

    // 6. Stream processing loop
    debug!(conv_id = %conv_id, "Entering stream processing loop");
    use futures::StreamExt;

    loop {
        // Check cancellation before each iteration
        if cancel_flag.load(Ordering::Relaxed) {
            debug!(conv_id = %conv_id, "Stream cancelled via cancellation token");
            break;
        }

        tokio::select! {
            biased;
            // Handle invoke_agent progress events first (sub-agent visualisation)
            Some(progress) = progress_rx.recv() => {
                use chatty_core::tools::invoke_agent_tool::InvokeAgentProgress;
                match progress {
                    InvokeAgentProgress::Started {
                        agent_name,
                        prompt,
                        source,
                    } => {
                        let label = format!("[Agent: {}] {}", agent_name, prompt);
                        cx.update_global::<ConversationsStore, _>(|store, _cx| {
                            if let Some(conv) = store.get_conversation_mut(&conv_id) {
                                conv.start_sub_agent_progress(&label, source.clone());
                            }
                        })
                        .map_err(|e| warn!(error = ?e, conv_id = %conv_id, "Failed to persist sub-agent start"))
                        .ok();
                        chat_view
                            .update(cx, |view, cx| {
                                if view.conversation_id().map(|id| id.as_str()) == Some(conv_id.as_str()) {
                                    view.start_sub_agent_progress(&label, source, cx);
                                }
                            })
                            .map_err(|e| warn!(error = ?e, conv_id = %conv_id, "Failed to update chat view with sub-agent start"))
                            .ok();
                    }
                    InvokeAgentProgress::Text(text) => {
                        cx.update_global::<ConversationsStore, _>(|store, _cx| {
                            if let Some(conv) = store.get_conversation_mut(&conv_id) {
                                conv.append_sub_agent_progress(&text);
                            }
                        })
                        .map_err(|e| warn!(error = ?e, conv_id = %conv_id, "Failed to persist sub-agent progress"))
                        .ok();
                        chat_view
                            .update(cx, |view, cx| {
                                if view.conversation_id().map(|id| id.as_str()) == Some(conv_id.as_str()) {
                                    view.append_sub_agent_progress(&text, cx);
                                }
                            })
                            .map_err(|e| warn!(error = ?e, conv_id = %conv_id, "Failed to update chat view with sub-agent progress"))
                            .ok();
                    }
                    InvokeAgentProgress::Finished { success, result } => {
                        cx.update_global::<ConversationsStore, _>(|store, _cx| {
                            if let Some(conv) = store.get_conversation_mut(&conv_id) {
                                conv.finalize_sub_agent_progress(success, result.clone());
                            }
                        })
                        .map_err(|e| warn!(error = ?e, conv_id = %conv_id, "Failed to persist sub-agent final state"))
                        .ok();
                        chat_view
                            .update(cx, |view, cx| {
                                if view.conversation_id().map(|id| id.as_str()) == Some(conv_id.as_str()) {
                                    view.finalize_sub_agent_progress(success, result, cx);
                                }
                            })
                            .map_err(|e| warn!(error = ?e, conv_id = %conv_id, "Failed to update chat view with sub-agent finish"))
                            .ok();
                    }
                }
                continue;
            }
            // Process LLM stream chunks
            chunk_result = stream.next() => {
                let chunk_result = match chunk_result {
                    Some(r) => r,
                    None => break,
                };

                match chunk_result {
                    Ok(StreamChunk::Text(ref text)) => {
                        // Update the Conversation model (source of truth for background streams)
                        cx.update_global::<ConversationsStore, _>(|store, _cx| {
                            if let Some(conv) = store.get_conversation_mut(&conv_id) {
                                conv.append_streaming_content(text);
                            }
                        })
                        .map_err(|e| warn!(error = ?e, "Failed to update conversation streaming content"))
                        .ok();
                        // Verbosity guard: flag if the model is writing a wall of text with no tools.
                        if !text_overflow_stop_requested
                            && loop_guard.on_text_chunk(text.len())
                        {
                            text_overflow_stop_requested = true;
                            debug!(conv_id = %conv_id,
                                "Text-only response exceeded verbosity limit; will inject brevity prompt after response completes.");
                        }
                    }
                    Ok(StreamChunk::TokenUsage { .. }) => {
                        // Token usage tracked by StreamManager
                    }
                    Ok(StreamChunk::Done) => {
                        debug!(conv_id = %conv_id, "Received Done chunk");
                        // If the model produced too much text without a tool call, queue a brevity prompt.
                        if text_overflow_stop_requested && pending_follow_up.is_none() {
                            pending_follow_up = Some(
                                "You produced a long response without any tool call. \
                                 If you have enough information, give your final answer now. \
                                 Otherwise, make a single focused tool call to get what you need."
                                    .to_string(),
                            );
                        }
                        if pending_follow_up.is_none() {
                            pending_follow_up = agent_task_controller.stream_end_follow_up();
                        }
                        // Forward to StreamManager before breaking
                        if let Some(ref sm) = stream_manager {
                            sm.update(cx, |sm: &mut crate::chatty::models::StreamManager, cx| {
                                sm.handle_chunk(&conv_id, StreamChunk::Done, cx)
                            })
                            .map_err(|e| warn!(error = ?e, conv_id = %conv_id, "Failed to forward Done chunk to StreamManager"))
                            .ok();
                        }
                        break;
                    }
                    Ok(StreamChunk::Error(ref err)) => {
                        error!(error = %err, conv_id = %conv_id, "Stream error");

                        // Detect authentication errors (401/Unauthorized)
                        if should_refresh_azure_auth(&provider_type, err) {
                            tracing::warn!("Detected Azure auth error - token likely expired");
                            if let Some(cache) = cx
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
                            provider_type,
                            chatty_core::settings::models::providers_store::ProviderType::OpenRouter
                        ) && is_auth_stream_error(err)
                        {
                            tracing::warn!(
                                "Detected OpenRouter authentication error - check the configured API key/header"
                            );
                        }
                    }
                    Ok(StreamChunk::ToolCallStarted { ref id, ref name }) => {
                        pending_tool_name.insert(id.clone(), name.clone());
                    }
                    Ok(StreamChunk::ToolCallInput { ref id, ref arguments }) => {
                        pending_tool_args.insert(id.clone(), arguments.clone());
                    }
                    Ok(StreamChunk::ToolCallResult { ref id, .. }) => {
                        let tool_name = pending_tool_name.remove(id).unwrap_or_default();
                        let tool_args = pending_tool_args.remove(id).unwrap_or_default();
                        if is_agent_todo_tool(&tool_name) {
                            let snapshot = agent_task_controller.snapshot();
                            cx.update_global::<ConversationsStore, _>(|store, _cx| {
                                if let Some(conv) = store.get_conversation_mut(&conv_id) {
                                    conv.set_agent_task_snapshot(Some(snapshot.clone()));
                                }
                            })
                            .map_err(|e| {
                                warn!(
                                    error = ?e,
                                    "Failed to persist agent todo panel snapshot in conversation state"
                                )
                            })
                            .ok();
                            chat_view
                                .update(cx, |view, cx| {
                                    if view.conversation_id().map(|id| id.as_str())
                                        == Some(conv_id.as_str())
                                    {
                                        view.set_agent_task_snapshot(snapshot.clone(), cx);
                                    }
                                })
                                .map_err(|e| {
                                    warn!(
                                        error = ?e,
                                        "Failed to update agent todo panel after todo tool result"
                                    )
                                })
                                .ok();
                            weak_ctrl
                                .update(&mut *cx, |app, cx| {
                                    app.persist_conversation(&conv_id, cx);
                                })
                                .map_err(|e| {
                                    warn!(
                                        error = ?e,
                                        "Failed to persist agent todo panel snapshot to disk"
                                    )
                                })
                                .ok();
                        }
                        if pending_follow_up.is_none()
                            && let Some(prompt) =
                                agent_task_controller.observe_tool_result(&tool_name)
                        {
                            debug!(
                                conv_id = %conv_id,
                                "Agent todo protocol: multiple tool results observed before write_todos"
                            );
                            cancel_flag.store(true, Ordering::Relaxed);
                            pending_follow_up = Some(prompt);
                        }
                        if let Some(pivot) = loop_guard.on_tool_completed(&tool_name, &tool_args) {
                            debug!(conv_id = %conv_id, pivot = %pivot,
                                "AgentLoopGuard loop detected; cancelling stream");
                            cancel_flag.store(true, Ordering::Relaxed);
                            pending_follow_up = Some(pivot);
                        }
                    }
                    Ok(_) => {
                        // ApprovalRequested, ApprovalResolved, TokenUsage, ToolCallError: no local state
                    }
                    Err(ref e) => {
                        error!(error = %e, conv_id = %conv_id, "Stream error");
                    }
                }

                // Forward ALL chunks to StreamManager (emits events for UI subscription)
                match chunk_result {
                    Ok(chunk) => {
                        let is_break = matches!(chunk, StreamChunk::Done | StreamChunk::Error(_));
                        if matches!(chunk, StreamChunk::Error(_)) {
                            // Same terminal path as the Err arm below: the manager
                            // drops the stream, so keep the trace and skip the
                            // redundant finalize afterwards.
                            let trace_json = extract_trace_json(&chat_view, &conv_id, cx);
                            stream_errored = true;
                            if let Some(ref sm) = stream_manager {
                                sm.update(cx, |sm: &mut crate::chatty::models::StreamManager, _cx| {
                                    sm.set_trace(&conv_id, trace_json);
                                })
                                .map_err(|e| warn!(error = ?e, "Failed to set trace before error"))
                                .ok();
                            }
                        }
                        if let Some(ref sm) = stream_manager {
                            sm.update(cx, |sm: &mut crate::chatty::models::StreamManager, cx| {
                                sm.handle_chunk(&conv_id, chunk, cx)
                            })
                            .map_err(|e| warn!(error = ?e, "Failed to forward chunk to StreamManager"))
                            .ok();
                        }
                        if is_break {
                            break;
                        }
                    }
                    Err(e) => {
                        let message = e.to_string();

                        // A truncated tool call is a model defect, not a dead
                        // connection: hand the parse error back and let it retry.
                        //
                        // The cap has to live in conversation history, not in a
                        // local: this function is re-entered fresh for every
                        // injected follow-up, so a local flag would reset each
                        // time and the retry would never terminate. That is
                        // AGE-150 Defect 2, and it is easy to rebuild by accident.
                        if is_malformed_tool_call_error(&message)
                            && pending_follow_up.is_none()
                            && !already_asked_to_retry(&conv_id, cx)
                        {
                            warn!(conv_id = %conv_id, error = %message,
                                "Malformed tool-call JSON; asking the model to retry");
                            pending_follow_up = Some(MALFORMED_TOOL_CALL_FOLLOW_UP.to_string());
                        }

                        // Keep the failed turn's tool calls in the transcript.
                        let trace_json = extract_trace_json(&chat_view, &conv_id, cx);
                        stream_errored = true;
                        if let Some(ref sm) = stream_manager {
                            sm.update(cx, |sm: &mut crate::chatty::models::StreamManager, cx| {
                                sm.set_trace(&conv_id, trace_json);
                                sm.handle_chunk(&conv_id, StreamChunk::Error(message), cx);
                            })
                            .map_err(|e| warn!(error = ?e, "Failed to forward error to StreamManager"))
                            .ok();
                        }
                        break;
                    }
                }
            } // end of stream.next() branch
        } // end of tokio::select!
    } // end of loop

    // Drain remaining progress events after stream ends
    while let Ok(progress) = progress_rx.try_recv() {
        use chatty_core::tools::invoke_agent_tool::InvokeAgentProgress;
        match progress {
            InvokeAgentProgress::Text(text) => {
                cx.update_global::<ConversationsStore, _>(|store, _cx| {
                    if let Some(conv) = store.get_conversation_mut(&conv_id) {
                        conv.append_sub_agent_progress(&text);
                    }
                })
                .map_err(|e| warn!(error = ?e, conv_id = %conv_id, "Failed to persist drained sub-agent progress"))
                .ok();
                chat_view
                    .update(cx, |view, cx| {
                        if view.conversation_id().map(|id| id.as_str()) == Some(conv_id.as_str()) {
                            view.append_sub_agent_progress(&text, cx);
                        }
                    })
                    .map_err(|e| warn!(error = ?e, conv_id = %conv_id, "Failed to update chat view with drained sub-agent progress"))
                    .ok();
            }
            InvokeAgentProgress::Finished { success, result } => {
                cx.update_global::<ConversationsStore, _>(|store, _cx| {
                    if let Some(conv) = store.get_conversation_mut(&conv_id) {
                        conv.finalize_sub_agent_progress(success, result.clone());
                    }
                })
                .map_err(|e| warn!(error = ?e, conv_id = %conv_id, "Failed to persist drained sub-agent final state"))
                .ok();
                chat_view
                    .update(cx, |view, cx| {
                        if view.conversation_id().map(|id| id.as_str()) == Some(conv_id.as_str()) {
                            view.finalize_sub_agent_progress(success, result, cx);
                        }
                    })
                    .map_err(|e| warn!(error = ?e, conv_id = %conv_id, "Failed to update chat view with drained sub-agent finish"))
                    .ok();
            }
            _ => {}
        }
    }

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
        weak_ctrl
            .update(&mut *cx, |app, cx| {
                app.send_protocol_follow_up(follow_up, cx);
            })
            .map_err(|e| warn!(error = ?e, "Failed to inject protocol follow-up"))
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
    use super::{MALFORMED_TOOL_CALL_FOLLOW_UP, is_malformed_tool_call_error};

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
}
