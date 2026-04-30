use super::*;

impl ChattyApp {
    /// Send a message to the LLM and stream the response.
    ///
    /// Spawns an async task that:
    /// 1. Ensures a conversation exists (creates one if needed)
    /// 2. Sets up UI with user message + assistant placeholder
    /// 3. Filters attachments based on provider capabilities
    /// 4. Runs the stream loop (forwards chunks to StreamManager)
    /// 5. Extracts trace and calls `finalize_stream()` on StreamManager
    ///
    /// UI updates, finalization, title generation, token usage, and persistence
    /// are handled by `handle_stream_manager_event()` reacting to StreamManager events.
    pub(super) fn send_message(
        &mut self,
        message: String,
        attachments: Vec<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        debug!(message = %message, attachment_count = attachments.len(), "send_message called");

        // Block message sending until app is ready (initial conversation created/loaded)
        if !self.is_ready {
            debug!("Not ready yet, ignoring message");
            return;
        }

        let chat_view = self.chat_view.clone();
        let sidebar = self.sidebar_view.clone();
        let app_entity = cx.entity();

        // Get the conversation ID for task tracking
        // If no conversation exists, we'll create one inside the async block
        let conv_id_for_task = cx.global::<ConversationsStore>().active_id().cloned();
        let needs_conversation_creation = conv_id_for_task.is_none();

        // Get pending artifacts handle for existing conversations (for stream registration)
        let pending_artifacts_for_registration = conv_id_for_task.as_ref().and_then(|id| {
            cx.global::<ConversationsStore>()
                .get_conversation(id)
                .map(|c| c.pending_artifacts())
        });

        // Create shared resolved ID tracker if we need to create a conversation
        let resolved_id = if needs_conversation_creation {
            Arc::new(Mutex::new(None))
        } else {
            Arc::new(Mutex::new(conv_id_for_task.clone()))
        };

        // Clone for the async closure to use
        let resolved_id_for_closure = resolved_id.clone();

        // Create cancellation token for graceful stream shutdown
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let cancel_flag_for_loop = cancel_flag.clone();

        // Get StreamManager entity for dual-write
        let stream_manager = cx.try_global::<GlobalStreamManager>().and_then(|g| g.get());

        // Get active conversation and send message
        debug!("Spawning async task for LLM call");
        let task = cx.spawn(async move |_weak, cx| -> anyhow::Result<()> {
                debug!("Async task started");

                // PHASE 1: Ensure conversation exists (create if needed)
                let conv_id: String = match cx
                    .update_global::<ConversationsStore, _>(|store, _| store.active_id().cloned())
                    .map_err(|e| anyhow::anyhow!(e.to_string()))?
                {
                    Some(id) => {
                        debug!(conv_id = %id, "Found active conversation");
                        id
                    }
                    None => {
                        debug!("No active conversation found, creating one");
                        let create_task = app_entity.update(cx, |app, cx| app.create_new_conversation(cx))
                            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                        match create_task.await {
                            Ok(id) => {
                                debug!(conv_id = %id, "Created new conversation");
                                // Update the shared resolved ID so load_conversation can find the active stream
                                if let Ok(mut resolved) = resolved_id_for_closure.lock() {
                                    *resolved = Some(id.clone());
                                    debug!(conv_id = %id, "Updated resolved conversation ID for pending task");
                                }
                                // Get conversation's PendingArtifacts handle before promoting
                                let pending_arts = cx.update(|cx| {
                                    cx.global::<ConversationsStore>()
                                        .get_conversation(&id)
                                        .map(|c| c.pending_artifacts())
                                }).ok().flatten();

                                // Promote the pending stream and wire up artifacts
                                if let Some(ref sm) = stream_manager {
                                    sm.update(cx, |mgr, _cx| {
                                        mgr.promote_pending(&id);
                                        // Wire the conversation's PendingArtifacts to the StreamState
                                        // so finalize_stream can drain them directly
                                        if let Some(arts) = pending_arts {
                                            mgr.set_pending_artifacts(&id, arts);
                                        }
                                    })
                                    .map_err(|e| debug!(error = ?e, "Failed to promote pending stream"))
                                    .ok();
                                }
                                id
                            }
                            Err(e) => {
                                error!(error = ?e, "Failed to create conversation");

                                // Cancel pending stream on error
                                if let Some(ref sm) = stream_manager {
                                    sm.update(cx, |mgr, cx| {
                                        mgr.cancel_pending(cx);
                                    })
                                    .map_err(|e| debug!(error = ?e, "Failed to cancel pending stream on error"))
                                    .ok();
                                }

                                return Err(e);
                            }
                        }
                    }
                };

                // PHASE 2: Initialize UI with user and assistant messages
                // and add the user/assistant messages AFTER conversation exists
                chat_view.update(cx, |view, cx| {
                    view.set_conversation_id(conv_id.clone(), cx);
                    // Add user message to UI
                    view.add_user_message(message.clone(), attachments.clone(), cx);
                    debug!("User message added to UI");
                    // Start assistant message in UI
                    view.start_assistant_message(cx);
                    debug!("Assistant message started");
                    cx.notify();
                }).map_err(|e| anyhow::anyhow!(e.to_string()))?;
                debug!(conv_id = %conv_id, "Set conversation ID on chat view");

                // Force sidebar to re-render by notifying it explicitly
                // This ensures the new conversation appears immediately
                sidebar.update(cx, |_sidebar, cx| {
                    cx.notify();
                }).map_err(|e| debug!(error = ?e, "Failed to refresh sidebar after creating conversation"))
                .ok();

                let needs_agent_workspace_refresh = cx
                    .update_global::<ConversationsStore, _>(|store, cx| {
                        let settings = cx.global::<ExecutionSettingsModel>();
                        store.get_conversation(&conv_id).map(|conv| {
                            let effective_workspace_dir = conv
                                .working_dir()
                                .cloned()
                                .or_else(|| settings.workspace_dir.as_ref().map(PathBuf::from));
                            conv.agent_workspace_dir().cloned() != effective_workspace_dir
                        })
                    })
                    .map_err(|e| anyhow::anyhow!(e.to_string()))?
                    .unwrap_or(false);

                if needs_agent_workspace_refresh {
                    info!(
                        conv_id = %conv_id,
                        "Refreshing conversation agent before send to apply latest workspace"
                    );
                    rebuild_conversation_agent(&conv_id, cx).await?;
                }

                // Extract agent, history, model_id, and capabilities synchronously
                let (agent, history, _model_id, provider_supports_pdf, provider_supports_images, conv_entries, invoke_agent_progress_slot) = cx
                    .update_global::<ConversationsStore, _>(|store, cx| {
                        if let Some(conv) = store.get_conversation(&conv_id) {
                            let model_id = conv.model_id().to_string();

                            // Get capabilities from ModelsModel
                            let (supports_pdf, supports_images) = cx
                                .global::<ModelsModel>()
                                .get_model(&model_id)
                                .map(|m| (m.supports_pdf, m.supports_images))
                                .unwrap_or((false, false)); // Safe fallback if model not found

                            // Clear any leftover artifacts from a previous stream
                            if let Ok(mut artifacts) = conv.pending_artifacts().lock() {
                                artifacts.clear();
                            }

                            Ok((
                                conv.agent().clone(),
                                conv.messages(),
                                model_id,
                                supports_pdf,
                                supports_images,
                                conv.entries().to_vec(),
                                conv.invoke_agent_progress_slot(),
                            ))
                        } else {
                            Err(anyhow::anyhow!(
                                "Could not find conversation after creation/lookup"
                            ))
                        }
                    })
                    .map_err(|e| anyhow::anyhow!(e.to_string()))??;

                // PHASE 3: Prepare user content and start LLM stream
                let mut contents = vec![rig::message::UserContent::Text(
                    rig::completion::message::Text {
                        text: message.clone(),
                    },
                )];

                // Convert file attachments to UserContent
                // Filter based on model capabilities to prevent panics in rig-core
                for path in &attachments {
                    let is_pdf = path.extension().and_then(|e| e.to_str()) == Some("pdf");
                    if is_pdf && !provider_supports_pdf {
                        warn!(?path, "Skipping PDF attachment: provider does not support PDFs");
                        continue;
                    }
                    if !is_pdf && !provider_supports_images {
                        warn!(?path, "Skipping image attachment: provider does not support images");
                        continue;
                    }
                    match attachment_to_user_content(path).await {
                        Ok(content) => contents.push(content),
                        Err(e) => warn!(?path, error = ?e, "Failed to convert attachment"),
                    }
                }

                // Include the most recent assistant-generated attachments so the LLM
                // can reference displayed images/PDFs in follow-up questions.
                let assistant_att_paths = select_recent_assistant_attachments(
                    &conv_entries,
                    provider_supports_images,
                    provider_supports_pdf,
                );
                for path in &assistant_att_paths {
                    match attachment_to_user_content(path).await {
                        Ok(content) => contents.push(content),
                        Err(e) => warn!(
                            ?path,
                            error = ?e,
                            "Failed to include assistant attachment"
                        ),
                    }
                }

                // PHASE 4: Run shared LLM stream (approval setup, streaming, finalization)
                run_llm_stream(
                    LlmStreamParams {
                        conv_id,
                        agent,
                        history,
                        user_contents: contents,
                        add_user_message_to_model: true,
                        attachment_paths: attachments,
                        chat_view,
                        stream_manager,
                        cancel_flag: cancel_flag_for_loop,
                        invoke_agent_progress_slot,
                    },
                    cx,
                )
                .await
            });

        // Register stream with StreamManager (owns task + cancel flag)
        if let Some(manager) = cx.try_global::<GlobalStreamManager>().and_then(|g| g.get()) {
            if let Some(ref conv_id) = conv_id_for_task {
                manager.update(cx, |mgr, cx| {
                    mgr.register_stream(
                        conv_id.clone(),
                        task,
                        cancel_flag,
                        pending_artifacts_for_registration,
                        cx,
                    );
                });
            } else if needs_conversation_creation {
                // For new conversations, pending_artifacts will be available after
                // Conversation::new() creates them. We pass None here; the follow-up
                // logic falls back to checking the conversation's artifacts directly.
                manager.update(cx, |mgr, cx| {
                    mgr.register_pending_stream(task, resolved_id, cancel_flag, None, cx);
                });
                debug!("Registered pending stream until conversation is created");
            }
        } else {
            error!("StreamManager not available! Stream events will not be emitted.");
        }
    }

    /// Handle events from StreamManager for decoupled UI updates
    pub(super) fn handle_stream_manager_event(
        &mut self,
        event: &StreamManagerEvent,
        cx: &mut Context<Self>,
    ) {
        let chat_view = self.chat_view.clone();

        match event {
            StreamManagerEvent::StreamStarted { conversation_id } => {
                debug!(conv_id = %conversation_id, "StreamManager: stream started");

                // Protect this conversation from LRU eviction while streaming
                if conversation_id != "__pending__" {
                    cx.update_global::<ConversationsStore, _>(|store, _| {
                        store.mark_streaming(conversation_id);
                    });
                }

                // Set streaming UI state if this is the active conversation
                let conv_id = conversation_id.clone();
                cx.defer(move |cx| {
                    chat_view.update(cx, |view, cx| {
                        if view.conversation_id().map(|s| s.as_str()) == Some(conv_id.as_str())
                            || conv_id == "__pending__"
                        {
                            view.chat_input_state().update(cx, |input, cx| {
                                input.set_streaming(true, cx);
                            });
                        }
                    });
                });
            }
            StreamManagerEvent::TextChunk {
                conversation_id,
                text,
            } => {
                let text = text.clone();
                chat_view.update(cx, |view, cx| {
                    if view.conversation_id() == Some(conversation_id) {
                        view.append_assistant_text(&text, cx);
                    }
                });
            }
            StreamManagerEvent::ToolCallStarted {
                conversation_id,
                id,
                name,
            } => {
                let id = id.clone();
                let name = name.clone();

                // Update Conversation model unconditionally (survives view switches)
                cx.update_global::<ConversationsStore, _>(|store, _cx| {
                    if let Some(conv) = store.get_conversation_mut(conversation_id) {
                        let text_before = conv.streaming_message().cloned().unwrap_or_default();
                        let display_name = friendly_tool_name(&name);
                        let tool_call = ToolCallBlock {
                            id: id.clone(),
                            tool_name: name.clone(),
                            display_name,
                            input: String::new(),
                            output: None,
                            output_preview: None,
                            state: ToolCallState::Running,
                            duration: None,
                            text_before,
                            source: classify_tool_source(&name),
                            execution_engine: chatty_core::models::message_types::classify_initial_execution_engine(&name),
                        };
                        let trace = conv.ensure_streaming_trace();
                        let index = trace.items.len();
                        trace.add_tool_call(tool_call);
                        trace.set_active_tool(index);
                    }
                });

                if name == "invoke_agent" {
                    // Suppress ToolCallBlock in the UI — the sub-agent progress
                    // system will handle visualisation via the progress channel.
                    self.active_invoke_agent_ids.insert(id);
                } else {
                    let source = classify_tool_source(&name);
                    chat_view.update(cx, |view, cx| {
                        if view.conversation_id() == Some(conversation_id) {
                            view.handle_tool_call_started(id, name, source, cx);
                        }
                    });
                }
            }
            StreamManagerEvent::ToolCallInput {
                conversation_id,
                id,
                arguments,
            } => {
                let id = id.clone();
                let arguments = arguments.clone();

                // Update Conversation model unconditionally
                cx.update_global::<ConversationsStore, _>(|store, _cx| {
                    if let Some(conv) = store.get_conversation_mut(conversation_id)
                        && let Some(trace) = conv.streaming_trace_mut()
                    {
                        let args = arguments.clone();
                        if !trace.update_tool_call(&id, |tc| {
                            tc.execution_engine =
                                chatty_core::models::message_types::predict_execution_engine(
                                    &tc.tool_name,
                                    &args,
                                )
                                .or(tc.execution_engine);
                            tc.input = args;
                        }) {
                            warn!(tool_id = %id, "ToolCallInput: tool call not found in model trace");
                        }
                    }
                });

                if !self.active_invoke_agent_ids.contains(&id) {
                    chat_view.update(cx, |view, cx| {
                        if view.conversation_id() == Some(conversation_id) {
                            view.handle_tool_call_input(id, arguments, cx);
                        }
                    });
                }
            }
            StreamManagerEvent::ToolCallResult {
                conversation_id,
                id,
                result,
            } => {
                let id = id.clone();
                let result = result.clone();

                // Update Conversation model unconditionally
                cx.update_global::<ConversationsStore, _>(|store, _cx| {
                    if let Some(conv) = store.get_conversation_mut(conversation_id)
                        && let Some(trace) = conv.streaming_trace_mut()
                    {
                        let res = result.clone();
                        let is_denied = is_denial_result(&res);
                        if !trace.update_tool_call(&id, |tc| {
                            tc.execution_engine =
                                chatty_core::models::message_types::detect_execution_engine(
                                    &tc.tool_name,
                                    &res,
                                );
                            tc.output = Some(res.clone());
                            tc.state = if is_denied {
                                ToolCallState::Error("Denied by user".to_string())
                            } else {
                                ToolCallState::Success
                            };
                        }) {
                            warn!(tool_id = %id, "ToolCallResult: tool call not found in model trace");
                        }
                        trace.clear_active_tool();
                    }
                });

                if self.active_invoke_agent_ids.remove(&id) {
                    // invoke_agent result — sub-agent progress already finalized via
                    // the progress channel; skip creating a ToolCallBlock result.
                } else {
                    chat_view.update(cx, |view, cx| {
                        if view.conversation_id() == Some(conversation_id) {
                            view.handle_tool_call_result(id, result, cx);
                        }
                    });
                }
            }
            StreamManagerEvent::ToolCallError {
                conversation_id,
                id,
                error,
            } => {
                let id = id.clone();
                let error = error.clone();

                // Update Conversation model unconditionally
                cx.update_global::<ConversationsStore, _>(|store, _cx| {
                    if let Some(conv) = store.get_conversation_mut(conversation_id)
                        && let Some(trace) = conv.streaming_trace_mut()
                    {
                        let err = error.clone();
                        if !trace.update_tool_call(&id, |tc| {
                            tc.state = ToolCallState::Error(err);
                        }) {
                            warn!(tool_id = %id, "ToolCallError: tool call not found in model trace");
                        }
                        trace.clear_active_tool();
                    }
                });

                if self.active_invoke_agent_ids.remove(&id) {
                    // invoke_agent error — sub-agent progress handles error
                    // finalization via the progress channel.
                } else {
                    chat_view.update(cx, |view, cx| {
                        if view.conversation_id() == Some(conversation_id) {
                            view.handle_tool_call_error(id, error, cx);
                        }
                    });
                }
            }
            StreamManagerEvent::ApprovalRequested {
                conversation_id,
                id,
                command,
                is_sandboxed,
            } => {
                debug!(id = %id, command = %command, sandboxed = is_sandboxed, "StreamManager: approval requested");
                let id = id.clone();
                let command = command.clone();
                let is_sandboxed = *is_sandboxed;

                // Update Conversation model unconditionally
                cx.update_global::<ConversationsStore, _>(|store, _cx| {
                    if let Some(conv) = store.get_conversation_mut(conversation_id) {
                        let approval = ApprovalBlock {
                            id: id.clone(),
                            command: command.clone(),
                            is_sandboxed,
                            state: ApprovalState::Pending,
                            created_at: std::time::SystemTime::now(),
                        };
                        let trace = conv.ensure_streaming_trace();
                        let index = trace.items.len();
                        trace.add_approval(approval);
                        trace.set_active_tool(index);
                    }
                });

                chat_view.update(cx, |view, cx| {
                    if view.conversation_id() == Some(conversation_id) {
                        view.handle_approval_requested(id, command, is_sandboxed, cx);
                    }
                });
            }
            StreamManagerEvent::ApprovalResolved {
                conversation_id,
                id,
                approved,
            } => {
                debug!(id = %id, approved = approved, "StreamManager: approval resolved");
                let id = id.clone();
                let approved = *approved;

                // Update Conversation model unconditionally
                cx.update_global::<ConversationsStore, _>(|store, _cx| {
                    if let Some(conv) = store.get_conversation_mut(conversation_id)
                        && let Some(trace) = conv.streaming_trace_mut()
                    {
                        let new_state = if approved {
                            ApprovalState::Approved
                        } else {
                            ApprovalState::Denied
                        };
                        trace.update_approval_state(&id, new_state);
                        trace.clear_active_tool();
                    }
                });

                chat_view.update(cx, |view, cx| {
                    if view.conversation_id() == Some(conversation_id) {
                        view.handle_approval_resolved(&id, approved, cx);
                    }
                });
            }
            StreamManagerEvent::TokenUsage {
                conversation_id: _,
                input_tokens: _,
                output_tokens: _,
            } => {
                // Token usage is handled during stream finalization, not per-chunk
            }
            StreamManagerEvent::StreamEnded {
                conversation_id,
                status,
                token_usage,
                trace_json,
                pending_artifacts,
                api_turn_count,
            } => {
                debug!(conv_id = %conversation_id, status = ?status, "StreamManager: stream ended");

                // Allow this conversation to be evicted again
                if conversation_id != "__pending__" {
                    cx.update_global::<ConversationsStore, _>(|store, _| {
                        store.unmark_streaming(conversation_id);
                    });
                }

                // Update UI streaming state
                chat_view.update(cx, |view, cx| {
                    if view.conversation_id() == Some(conversation_id)
                        || conversation_id == "__pending__"
                    {
                        view.chat_input_state().update(cx, |input, cx| {
                            input.set_streaming(false, cx);
                        });
                    }
                });

                match status {
                    StreamStatus::Completed => {
                        // Drain artifacts queued by AddAttachmentTool.
                        // Primary source: StreamState.pending_artifacts (set via set_pending_artifacts).
                        // Fallback: drain directly from the conversation's pending_artifacts
                        // (for edge cases where set_pending_artifacts wasn't wired).
                        let artifacts = pending_artifacts
                            .clone()
                            .or_else(|| {
                                cx.try_global::<ConversationsStore>()
                                    .and_then(|store| store.get_conversation(conversation_id))
                                    .and_then(|conv| {
                                        conv.pending_artifacts()
                                            .lock()
                                            .ok()
                                            .map(|mut v| v.drain(..).collect::<Vec<_>>())
                                    })
                                    .filter(|v| !v.is_empty())
                                    .inspect(|v| {
                                        warn!(
                                            conv_id = %conversation_id,
                                            count = v.len(),
                                            "Artifacts missing from event, recovered via fallback drain"
                                        );
                                    })
                            })
                            .unwrap_or_default();

                        self.finalize_completed_stream(
                            conversation_id,
                            *token_usage,
                            trace_json.clone(),
                            artifacts.clone(),
                            *api_turn_count,
                            cx,
                        );

                        // Update display message with attachment paths
                        if !artifacts.is_empty() {
                            chat_view.update(cx, |view, cx| {
                                if view.conversation_id() == Some(conversation_id)
                                    || conversation_id == "__pending__"
                                {
                                    view.set_last_assistant_attachments(artifacts, cx);
                                }
                            });
                        }
                    }
                    StreamStatus::Cancelled => {
                        // Pending streams have no conversation yet — only UI reset (done above)
                        if conversation_id != "__pending__" {
                            self.finalize_stopped_stream(conversation_id, trace_json.clone(), cx);
                        }
                    }
                    _ => {}
                }

                // Clear streaming message and trace from Conversation model
                cx.update_global::<ConversationsStore, _>(|store, _cx| {
                    if let Some(conv) = store.get_conversation_mut(conversation_id) {
                        conv.set_streaming_message(None);
                        conv.set_streaming_trace(None);
                        conv.set_streaming_sub_agent_trace(None);
                    }
                });
            }
        }
    }

    /// Stop the currently active stream for the current conversation.
    /// Delegates to StreamManager which sets the cancellation token and emits StreamEnded.
    pub fn stop_stream(&mut self, cx: &mut Context<Self>) {
        let conv_id = cx
            .try_global::<ConversationsStore>()
            .and_then(|store| store.active_id().cloned())
            .unwrap_or_else(|| "__pending__".to_string());

        debug!(conv_id = %conv_id, "stop_stream called");

        // Extract trace before stopping.
        // Try ChatView first, fall back to Conversation model streaming_trace.
        let trace_from_view = self
            .chat_view
            .update(cx, |view, _cx| view.extract_current_trace());
        let trace = trace_from_view.or_else(|| {
            cx.try_global::<ConversationsStore>()
                .and_then(|store| store.get_conversation(&conv_id))
                .and_then(|conv| conv.streaming_trace().cloned())
        });
        let trace_json = trace.and_then(|trace| match serde_json::to_value(&trace) {
            Ok(val) => Some(val),
            Err(e) => {
                error!(error = ?e, "Failed to serialize trace in stop_stream");
                None
            }
        });

        // Set trace on StreamManager so it's included in the StreamEnded event
        if let Some(manager) = cx.try_global::<GlobalStreamManager>().and_then(|g| g.get()) {
            manager.update(cx, |mgr, _cx| {
                mgr.set_trace(&conv_id, trace_json);
            });

            // Delegate cancellation to StreamManager
            // This sets cancel flag, drops task, emits StreamEnded(Cancelled)
            manager.update(cx, |mgr, cx| {
                mgr.stop_stream(&conv_id, cx);
            });
        }

        cx.notify();
    }

    /// Handle the finalization of a successfully completed stream.
    /// Called from handle_stream_manager_event when StreamEnded with Completed status.
    ///
    /// Reads the accumulated response text from `ConversationsStore.streaming_message`
    /// (the single source of truth for streaming content).
    ///
    /// Performs:
    /// 1. Finalize assistant message in UI (stop streaming animation)
    /// 2. Save response + trace to conversation model
    /// 3. Process token usage and calculate cost
    /// 4. Generate title for first exchange (async)
    /// 5. Update sidebar with title/cost
    /// 6. Persist conversation to disk
    fn finalize_completed_stream(
        &mut self,
        conversation_id: &str,
        token_usage: Option<(u32, u32)>,
        trace_json: Option<serde_json::Value>,
        artifact_paths: Vec<PathBuf>,
        api_turn_count: u32,
        cx: &mut Context<Self>,
    ) {
        let chat_view = self.chat_view.clone();
        let sidebar = self.sidebar_view.clone();
        let conv_id = conversation_id.to_string();

        // 1. Finalize UI - stop streaming animation
        chat_view.update(cx, |view, cx| {
            if view.conversation_id().map(|s| s.as_str()) == Some(conv_id.as_str()) {
                view.finalize_assistant_message(cx);
            }
        });

        // 2. Read response text from ConversationsStore (single source of truth),
        //    finalize in conversation model, check if title gen needed, and
        //    extract model_id for pricing lookup (avoids a second global access later).
        let (should_generate_title, assistant_history_index, model_id_opt) =
            cx.update_global::<ConversationsStore, _>(|store, _cx| {
                if let Some(conv) = store.get_conversation_mut(&conv_id) {
                    let response_text = conv
                        .streaming_message()
                        .cloned()
                        .unwrap_or_default();
                    let has_trace = trace_json.is_some();
                    let model_id = conv.model_id().to_string();
                    conv.finalize_response(response_text, artifact_paths, trace_json);
                    let msg_count = conv.message_count();
                    let traces_len = conv.entries().len();
                    // The assistant message was just pushed; its index is msg_count - 1
                    let assistant_idx = msg_count.saturating_sub(1);
                    let should_gen = msg_count == 2 && conv.title() == "New Chat";
                    debug!(conv_id = %conv_id, msg_count, traces_len, has_trace, should_gen, "Response finalized in conversation");
                    (should_gen, Some(assistant_idx), Some(model_id))
                } else {
                    error!(conv_id = %conv_id, "Could not find conversation to finalize");
                    (false, None, None)
                }
            });

        // 2b. Set history_index on the last assistant DisplayMessage so feedback
        //     clicks on freshly-streamed messages are properly persisted.
        if let Some(h_idx) = assistant_history_index {
            chat_view.update(cx, |view, cx| {
                if view.conversation_id().map(|s| s.as_str()) == Some(conv_id.as_str()) {
                    view.set_last_assistant_history_index(h_idx, cx);
                }
            });
        }

        // 3. Process token usage — always record tokens, optionally calculate cost
        if let Some((input_tokens, output_tokens)) = token_usage {
            debug!(
                input_tokens,
                output_tokens, api_turn_count, "Processing token usage"
            );

            let mut usage =
                TokenUsage::with_turn_count(input_tokens, output_tokens, api_turn_count);

            // Calculate cost if pricing is configured for this model
            if let Some(ref model_id) = model_id_opt {
                let pricing = cx.update_global::<ModelsModel, _>(|models, _cx| {
                    models.get_model(model_id).and_then(|model| {
                        match (
                            model.cost_per_million_input_tokens,
                            model.cost_per_million_output_tokens,
                        ) {
                            (Some(input_cost), Some(output_cost)) => {
                                Some((input_cost, output_cost))
                            }
                            _ => None,
                        }
                    })
                });

                if let Some((cost_per_million_input, cost_per_million_output)) = pricing {
                    usage.calculate_cost(cost_per_million_input, cost_per_million_output);
                }
            }

            cx.update_global::<ConversationsStore, _>(|store, _cx| {
                if let Some(conv) = store.get_conversation_mut(&conv_id) {
                    conv.add_token_usage(usage);
                }
                // Sync metadata so sidebar cost matches the live conversation cost
                // after every turn (not just the first turn where title is generated).
                let cost_and_title = store.get_conversation(&conv_id).map(|c| {
                    (
                        c.token_usage().total_estimated_cost_usd,
                        c.title().to_string(),
                    )
                });
                if let Some((cost, title)) = cost_and_title {
                    let now_ts = std::time::SystemTime::now()
                        .duration_since(std::time::SystemTime::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs() as i64;
                    store.upsert_metadata(&conv_id, &title, cost, now_ts);
                }
            });

            // Overlay the pre-send estimate with the actual API-reported token counts.
            // This lets the popover show both the estimate (computed before send) and the
            // real numbers from the provider in the same snapshot.
            if cx.has_global::<GlobalTokenBudget>() {
                cx.global::<GlobalTokenBudget>()
                    .update_with_actuals(input_tokens, output_tokens);
            }

            // 3c. Auto-summarize when context is critically full and the setting is enabled.
            // Reads the (now-patched) snapshot to check real utilization from the provider.
            let should_auto_summarize = cx
                .try_global::<TokenTrackingSettings>()
                .map(|s| s.auto_summarize)
                .unwrap_or(false);

            if should_auto_summarize {
                let is_critical = cx
                    .try_global::<GlobalTokenBudget>()
                    .and_then(|g| g.receiver.borrow().clone())
                    .map(|snap| snap.status().is_critical())
                    .unwrap_or(false);

                if is_critical {
                    let conv_id_for_summary = conv_id.clone();
                    cx.spawn(async move |_weak, cx| {
                        let data = cx
                            .update_global::<ConversationsStore, _>(|store, _cx| {
                                store
                                    .get_conversation(&conv_id_for_summary)
                                    .map(|conv| (conv.agent().clone(), conv.messages()))
                            })
                            .map_err(|e| anyhow::anyhow!(e.to_string()))?;

                        if let Some((agent, history)) = data {
                            // Compute midpoint from the captured snapshot so it stays
                            // consistent with the history slice passed to the LLM, even
                            // if new messages arrive while summarization is in flight.
                            let midpoint = history.len() / 2;
                            match summarize_oldest_half(&agent, &history).await {
                                Ok(result) => {
                                    info!(
                                        conv_id = %conv_id_for_summary,
                                        messages_summarized = result.messages_summarized,
                                        estimated_tokens_freed = result.estimated_tokens_freed,
                                        "Auto-summarization complete"
                                    );
                                    cx.update_global::<ConversationsStore, _>(|store, _cx| {
                                        if let Some(conv) =
                                            store.get_conversation_mut(&conv_id_for_summary)
                                        {
                                            conv.replace_history(result.new_history, midpoint);
                                        }
                                    })
                                    .map_err(
                                        |e| warn!(error = ?e, "Failed to apply auto-summarization"),
                                    )
                                    .ok();
                                }
                                Err(e) => {
                                    warn!(error = ?e, "Auto-summarization failed");
                                }
                            }
                        }

                        Ok::<_, anyhow::Error>(())
                    })
                    .detach();
                }
            }
        }

        // 4. Update sidebar with latest data
        self.refresh_sidebar(cx);

        // 5. Generate title for first exchange (async) + persist
        if should_generate_title {
            let conv_id_for_title = conv_id.clone();
            let sidebar_for_title = sidebar.clone();

            cx.spawn(async move |_weak, cx| {
                // Get agent and history for title generation
                let title_data = cx
                    .update_global::<ConversationsStore, _>(|store, _cx| {
                        store
                            .get_conversation(&conv_id_for_title)
                            .map(|conv| (conv.agent().clone(), conv.messages()))
                    })
                    .map_err(|e| anyhow::anyhow!(e.to_string()))?;

                if let Some((agent, history)) = title_data {
                    match generate_title(&agent, &history).await {
                        Ok(new_title) => {
                            debug!(title = %new_title, "Generated title");

                            cx.update_global::<ConversationsStore, _>(|store, _cx| {
                                if let Some(conv) = store.get_conversation_mut(&conv_id_for_title) {
                                    conv.set_title(new_title.clone());
                                }
                                // Compute cost separately to avoid simultaneous borrow
                                let cost = store
                                    .get_conversation(&conv_id_for_title)
                                    .map(|c| c.token_usage().total_estimated_cost_usd)
                                    .unwrap_or(0.0);
                                let now_ts = std::time::SystemTime::now()
                                    .duration_since(std::time::SystemTime::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_secs() as i64;
                                // Also update metadata so sidebar reflects the new title
                                store.upsert_metadata(&conv_id_for_title, &new_title, cost, now_ts);
                            })
                            .map_err(|e| warn!(error = ?e, "Failed to update conversation title"))
                            .ok();

                            // Update sidebar with new title from metadata
                            sidebar_for_title
                                .update(cx, |sidebar, cx| {
                                    let store = cx.global::<ConversationsStore>();
                                    let total = store.count();
                                    let convs = store.list_recent_metadata(sidebar.visible_limit());
                                    sidebar.set_conversations(convs, cx);
                                    sidebar.set_total_count(total);
                                })
                                .map_err(|e| {
                                    warn!(error = ?e, "Failed to update sidebar with new title")
                                })
                                .ok();
                        }
                        Err(e) => {
                            warn!(error = ?e, "Title generation failed");
                        }
                    }
                }

                Ok::<_, anyhow::Error>(())
            })
            .detach();
        }

        // 6. Persist to disk
        self.persist_conversation(&conv_id, cx);

        // 7. Auto-export ATIF if enabled in training settings
        if cx
            .try_global::<TrainingSettingsModel>()
            .map(|s| s.atif_auto_export)
            .unwrap_or(false)
        {
            self.export_conversation_atif(&conv_id, cx);
        }

        // 8. Auto-export JSONL (SFT + DPO) if enabled in training settings
        if cx
            .try_global::<TrainingSettingsModel>()
            .map(|s| s.jsonl_auto_export)
            .unwrap_or(false)
        {
            self.export_conversation_jsonl(&conv_id, cx);
        }
    }

    /// Handle the finalization of a stopped stream (partial response saving).
    /// Called from handle_stream_manager_event when StreamEnded with Cancelled status.
    ///
    /// Reads the accumulated partial response from `ConversationsStore.streaming_message`
    /// (the single source of truth for streaming content).
    fn finalize_stopped_stream(
        &mut self,
        conversation_id: &str,
        trace_json: Option<serde_json::Value>,
        cx: &mut Context<Self>,
    ) {
        let chat_view = self.chat_view.clone();
        let conv_id = conversation_id.to_string();

        // Mark the assistant message as cancelled in UI
        chat_view.update(cx, |view, cx| {
            if view.conversation_id().map(|s| s.as_str()) == Some(conv_id.as_str()) {
                view.mark_message_cancelled(cx);
            }
        });

        // Read partial response from ConversationsStore (single source of truth)
        // and save to conversation history — but ONLY if there's actual content.
        // An empty assistant message would cause LLM API errors (400 Bad Request)
        // on the next request.
        let assistant_history_index = cx.update_global::<ConversationsStore, _>(|store, _cx| {
            if let Some(conv) = store.get_conversation_mut(&conv_id) {
                let partial_text = conv.streaming_message().cloned().unwrap_or_default();

                if partial_text.is_empty() {
                    // No content was received before cancellation.
                    // Roll back the user message that triggered this stream to avoid
                    // a trailing user message with no assistant response, which would
                    // break the alternating User/Assistant pattern expected by LLM APIs.
                    let removed = conv.remove_last_user_message();
                    debug!(
                        conv_id = %conv_id,
                        user_msg_removed = removed,
                        "Stream cancelled with no content — skipped empty assistant message"
                    );
                    None
                } else {
                    conv.finalize_response(partial_text, Vec::new(), trace_json);
                    conv.set_streaming_message(None);
                    let idx = conv.message_count().saturating_sub(1);
                    debug!(conv_id = %conv_id, "Partial response saved to conversation after stop");
                    Some(idx)
                }
            } else {
                None
            }
        });

        // Set history_index on the cancelled assistant message for feedback persistence
        if let Some(h_idx) = assistant_history_index {
            chat_view.update(cx, |view, cx| {
                if view.conversation_id().map(|s| s.as_str()) == Some(conv_id.as_str()) {
                    view.set_last_assistant_history_index(h_idx, cx);
                }
            });
        }

        // Persist to disk
        self.persist_conversation(&conv_id, cx);
    }

    /// Handle feedback change: update ConversationsStore and persist
    pub(super) fn handle_feedback_changed(
        &self,
        history_index: usize,
        feedback: Option<MessageFeedback>,
        cx: &mut Context<Self>,
    ) {
        let conv_id = cx.global::<ConversationsStore>().active_id().cloned();

        if let Some(conv_id) = conv_id {
            cx.update_global::<ConversationsStore, _>(|store, _cx| {
                if let Some(conv) = store.get_conversation_mut(&conv_id) {
                    conv.set_message_feedback(history_index, feedback);
                }
            });
            self.persist_conversation(&conv_id, cx);
        }
    }

    /// Handle regeneration of the last assistant message.
    ///
    /// Records the original response as a DPO preference pair, removes the old
    /// assistant message from both model and UI, then re-streams using the existing
    /// conversation history (the user message is already in history, so it is NOT
    /// re-added). Uses the shared `run_llm_stream` helper for the streaming phase.
    pub(super) fn handle_regeneration(&mut self, history_index: usize, cx: &mut Context<Self>) {
        let conv_id = match cx.global::<ConversationsStore>().active_id().cloned() {
            Some(id) => id,
            None => return,
        };

        // PHASE 1: Remove old assistant message and record DPO pair
        let ok = cx.update_global::<ConversationsStore, _>(|store, _cx| {
            let conv = store.get_conversation_mut(&conv_id)?;

            if history_index == 0 || history_index >= conv.message_count() {
                return None;
            }

            let (original_text, original_timestamp) = conv.remove_last_assistant_message()?;

            conv.record_regeneration(
                history_index,
                original_text,
                original_timestamp.unwrap_or(0),
            );

            Some(())
        });

        if ok.is_none() {
            return;
        }

        // PHASE 2: Update UI — remove old assistant message, start fresh placeholder
        let chat_view = self.chat_view.clone();
        chat_view.update(cx, |view, cx| {
            view.remove_last_assistant_message(cx);
            view.start_assistant_message(cx);
        });

        // Persist the regeneration record before streaming
        self.persist_conversation(&conv_id, cx);

        // PHASE 3: Stream new response via shared helper
        let sidebar = self.sidebar_view.clone();
        let pending_artifacts = cx
            .global::<ConversationsStore>()
            .get_conversation(&conv_id)
            .map(|c| c.pending_artifacts());

        let cancel_flag = Arc::new(AtomicBool::new(false));
        let cancel_flag_for_loop = cancel_flag.clone();

        let stream_manager = cx.try_global::<GlobalStreamManager>().and_then(|g| g.get());

        let conv_id_for_task = conv_id.clone();
        let task = cx.spawn(async move |_weak, cx| -> anyhow::Result<()> {
            debug!(conv_id = %conv_id, "Regeneration: starting new stream");

            // Force sidebar to re-render
            sidebar
                .update(cx, |_sidebar, cx| {
                    cx.notify();
                })
                .map_err(|e| warn!(error = ?e, "Failed to refresh sidebar"))
                .ok();

            // Extract agent and history (ends with the user message after removal)
            let (agent, history, invoke_agent_progress_slot) = cx
                .update_global::<ConversationsStore, _>(|store, _cx| {
                    if let Some(conv) = store.get_conversation(&conv_id) {
                        if let Ok(mut artifacts) = conv.pending_artifacts().lock() {
                            artifacts.clear();
                        }
                        Ok((
                            conv.agent().clone(),
                            conv.messages(),
                            conv.invoke_agent_progress_slot(),
                        ))
                    } else {
                        Err(anyhow::anyhow!("Conversation not found for regeneration"))
                    }
                })
                .map_err(|e| anyhow::anyhow!(e.to_string()))??;

            // Split history: context (all but last) + user content (last message)
            let len = history.len();
            if len == 0 {
                return Err(anyhow::anyhow!("Empty history during regeneration"));
            }
            let history_context = history[..len - 1].to_vec();
            let user_contents = match &history[len - 1] {
                rig::completion::Message::User { content, .. } => {
                    content.iter().cloned().collect::<Vec<_>>()
                }
                _ => {
                    return Err(anyhow::anyhow!(
                        "Last message in history is not a user message"
                    ));
                }
            };

            // Run shared LLM stream (do NOT add user message — it's already in history)
            run_llm_stream(
                LlmStreamParams {
                    conv_id,
                    agent,
                    history: history_context,
                    user_contents,
                    add_user_message_to_model: false,
                    attachment_paths: vec![],
                    chat_view,
                    stream_manager,
                    cancel_flag: cancel_flag_for_loop,
                    invoke_agent_progress_slot,
                },
                cx,
            )
            .await
        });

        // Register stream with StreamManager
        if let Some(manager) = cx.try_global::<GlobalStreamManager>().and_then(|g| g.get()) {
            manager.update(cx, |mgr, cx| {
                mgr.register_stream(conv_id_for_task, task, cancel_flag, pending_artifacts, cx);
            });
        } else {
            error!("StreamManager not available for regeneration stream");
        }
    }
}

/// Parameters for the shared LLM stream processing.
struct LlmStreamParams {
    conv_id: String,
    agent: AgentClient,
    history: Vec<rig::completion::Message>,
    user_contents: Vec<rig::message::UserContent>,
    add_user_message_to_model: bool,
    attachment_paths: Vec<PathBuf>,
    chat_view: Entity<ChatView>,
    stream_manager: Option<Entity<crate::chatty::models::StreamManager>>,
    cancel_flag: Arc<AtomicBool>,
    invoke_agent_progress_slot: chatty_core::tools::invoke_agent_tool::InvokeAgentProgressSlot,
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
async fn run_llm_stream(params: LlmStreamParams, cx: &mut AsyncApp) -> anyhow::Result<()> {
    let LlmStreamParams {
        conv_id,
        agent,
        history,
        user_contents,
        add_user_message_to_model,
        attachment_paths,
        chat_view,
        stream_manager,
        cancel_flag,
        invoke_agent_progress_slot,
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
                .ok()
                .flatten();

            let settings = cx
                .update(|cx| {
                    cx.try_global::<crate::settings::models::TokenTrackingSettings>()
                        .cloned()
                })
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

    // 3. Call stream_prompt with user contents directly (no auto-context injection)
    let llm_user_contents = user_contents.clone();
    debug!(conv_id = %conv_id, "Calling stream_prompt()");
    let (mut stream, _user_message) = stream_prompt(
        &agent,
        &history,
        llm_user_contents,
        Some(approval_rx),
        Some(resolution_rx),
        max_agent_turns,
    )
    .await?;

    // 4. Optionally add user message to conversation model.
    if add_user_message_to_model {
        let user_message = rig::completion::Message::User {
            content: rig::OneOrMany::many(user_contents).map_err(|e| {
                anyhow::anyhow!("Failed to create user message from contents: {}", e)
            })?,
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
                    }
                    Ok(StreamChunk::TokenUsage { .. }) => {
                        // Token usage tracked by StreamManager
                    }
                    Ok(StreamChunk::Done) => {
                        debug!(conv_id = %conv_id, "Received Done chunk");
                        // Forward to StreamManager before breaking
                        if let Some(ref sm) = stream_manager {
                            sm.update(cx, |sm: &mut crate::chatty::models::StreamManager, cx| {
                                sm.handle_chunk(&conv_id, StreamChunk::Done, cx)
                            })
                            .ok();
                        }
                        break;
                    }
                    Ok(StreamChunk::Error(ref err)) => {
                        error!(error = %err, conv_id = %conv_id, "Stream error");

                        // Detect authentication errors (401/Unauthorized)
                        if err.contains("401") || err.contains("Unauthorized") {
                            tracing::warn!("Detected Azure auth error - token likely expired");
                            if let Some(cache) = cx
                                .update(|cx| {
                                    cx.try_global::<crate::chatty::auth::AzureTokenCache>()
                                        .cloned()
                                })
                                .ok()
                                .flatten()
                            {
                                if let Err(e) = cache.refresh_token().await {
                                    error!(error = ?e, "Failed to refresh Azure token after 401 error");
                                } else {
                                    tracing::info!("Azure token refreshed successfully.");
                                }
                            }
                        }
                    }
                    Ok(_) => {
                        // ToolCall*, Approval* chunks: no local state update needed
                    }
                    Err(ref e) => {
                        error!(error = %e, conv_id = %conv_id, "Stream error");
                    }
                }

                // Forward ALL chunks to StreamManager (emits events for UI subscription)
                match chunk_result {
                    Ok(chunk) => {
                        let is_break = matches!(chunk, StreamChunk::Done | StreamChunk::Error(_));
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
                        if let Some(ref sm) = stream_manager {
                            sm.update(cx, |sm: &mut crate::chatty::models::StreamManager, cx| {
                                sm.handle_chunk(&conv_id, StreamChunk::Error(e.to_string()), cx);
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

    // Try to extract trace from ChatView first (if this conversation is displayed).
    // Fall back to the streaming_trace from the Conversation model (if user switched away).
    let trace_from_view = chat_view
        .update(cx, |view, _cx| view.extract_current_trace())
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;

    let trace = trace_from_view.or_else(|| {
        cx.try_read_global::<ConversationsStore, _>(|store, _| {
            store
                .get_conversation(&conv_id)
                .and_then(|conv| conv.streaming_trace().cloned())
        })
        .flatten()
    });

    let trace_json = trace.and_then(|trace| match serde_json::to_value(&trace) {
        Ok(val) => {
            debug!(conv_id = %conv_id, items = trace.items.len(), "Trace serialized successfully");
            Some(val)
        }
        Err(e) => {
            error!(conv_id = %conv_id, error = ?e, "Failed to serialize trace in run_llm_stream");
            None
        }
    });

    if let Some(ref sm) = stream_manager {
        sm.update(cx, |sm: &mut crate::chatty::models::StreamManager, cx| {
            sm.set_trace(&conv_id, trace_json);
            sm.finalize_stream(&conv_id, cx);
        })
        .map_err(|e| warn!(error = ?e, "Failed to finalize stream in StreamManager"))
        .ok();
    }

    Ok(())
}

/// Select attachment paths from the most recent assistant message that the
/// current model can handle. Returns paths filtered by capability.
///
/// Used to include tool-generated images/PDFs in follow-up prompts so the
/// LLM can reference previously displayed files.
fn select_recent_assistant_attachments(
    entries: &[chatty_core::models::MessageEntry],
    supports_images: bool,
    supports_pdf: bool,
) -> Vec<PathBuf> {
    if !supports_images && !supports_pdf {
        return Vec::new();
    }
    for entry in entries.iter().rev() {
        if matches!(entry.message, rig::completion::Message::Assistant { .. })
            && !entry.attachment_paths.is_empty()
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
async fn attachment_to_user_content(path: &Path) -> anyhow::Result<rig::message::UserContent> {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();

    let data = tokio::fs::read(path).await?;
    let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &data);

    match ext.as_str() {
        "png" => Ok(rig::message::UserContent::image_base64(
            b64,
            Some(rig::completion::message::ImageMediaType::PNG),
            Some(rig::completion::message::ImageDetail::Auto),
        )),
        "jpg" | "jpeg" => Ok(rig::message::UserContent::image_base64(
            b64,
            Some(rig::completion::message::ImageMediaType::JPEG),
            Some(rig::completion::message::ImageDetail::Auto),
        )),
        "gif" => Ok(rig::message::UserContent::image_base64(
            b64,
            Some(rig::completion::message::ImageMediaType::GIF),
            Some(rig::completion::message::ImageDetail::Auto),
        )),
        "webp" => Ok(rig::message::UserContent::image_base64(
            b64,
            Some(rig::completion::message::ImageMediaType::WEBP),
            Some(rig::completion::message::ImageDetail::Auto),
        )),
        "svg" => Ok(rig::message::UserContent::image_base64(
            b64,
            Some(rig::completion::message::ImageMediaType::SVG),
            Some(rig::completion::message::ImageDetail::Auto),
        )),
        "pdf" => Ok(rig::message::UserContent::Document(
            rig::completion::message::Document {
                data: rig::completion::message::DocumentSourceKind::Base64(b64),
                media_type: Some(rig::completion::message::DocumentMediaType::PDF),
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
    use rig::OneOrMany;
    use rig::completion::message::{AssistantContent, Text};
    use rig::message::{Message, UserContent};

    fn user_msg(text: &str) -> Message {
        Message::User {
            content: OneOrMany::one(UserContent::text(text)),
        }
    }

    fn assistant_msg(text: &str) -> Message {
        Message::Assistant {
            id: None,
            content: OneOrMany::one(AssistantContent::Text(Text {
                text: text.to_string(),
            })),
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
