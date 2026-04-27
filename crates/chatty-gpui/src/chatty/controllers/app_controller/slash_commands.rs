use super::*;

impl ChattyApp {
    // -----------------------------------------------------------------------
    // Slash-command handlers
    // -----------------------------------------------------------------------

    /// Dispatch a slash command that was selected from the picker.
    pub(super) fn handle_slash_command(&mut self, command: String, cx: &mut Context<Self>) {
        debug!(command = %command, "handle_slash_command");
        match command.as_str() {
            "/clear" | "/new" => {
                info!("Slash command: start new conversation");
                self.start_new_conversation(cx);
            }
            "/compact" => {
                info!("Slash command: compact conversation");
                self.compact_conversation(cx);
            }
            "/context" => {
                info!("Slash command: show context usage");
                self.show_context_info(cx);
            }
            "/copy" => {
                info!("Slash command: copy last response");
                self.copy_last_response(cx);
            }
            "/cwd" => {
                info!("Slash command: show working directory");
                self.show_working_directory(cx);
            }
            other => {
                warn!(command = %other, "Unknown slash command received");
            }
        }
    }

    /// `/compact` — summarize the oldest half of the conversation history.
    fn compact_conversation(&mut self, cx: &mut Context<Self>) {
        let conv_id = match cx
            .try_global::<ConversationsStore>()
            .and_then(|s| s.active_id().cloned())
        {
            Some(id) => id,
            None => {
                self.chat_view.update(cx, |view, cx| {
                    view.add_info_message("No active conversation to compact.".to_string(), cx);
                });
                return;
            }
        };

        let data = cx.try_global::<ConversationsStore>().and_then(|store| {
            store
                .get_conversation(&conv_id)
                .map(|conv| (conv.agent().clone(), conv.messages()))
        });

        let Some((agent, history)) = data else {
            self.chat_view.update(cx, |view, cx| {
                view.add_info_message("Conversation not found.".to_string(), cx);
            });
            return;
        };

        if history.len() < 4 {
            self.chat_view.update(cx, |view, cx| {
                view.add_info_message(
                    "Conversation is too short to compact (need at least 4 messages).".to_string(),
                    cx,
                );
            });
            return;
        }

        let chat_view = self.chat_view.clone();
        let conv_id_clone = conv_id.clone();
        let midpoint = history.len() / 2;
        cx.spawn(
            async move |_weak, cx| match summarize_oldest_half(&agent, &history).await {
                Ok(result) => {
                    let msg = format!(
                        "Compacted conversation: summarized {} messages (~{} tokens freed).",
                        result.messages_summarized, result.estimated_tokens_freed
                    );
                    cx.update_global::<ConversationsStore, _>(|store, _cx| {
                        if let Some(conv) = store.get_conversation_mut(&conv_id_clone) {
                            conv.replace_history(result.new_history, midpoint);
                        }
                    })
                    .map_err(|e| warn!(error = ?e, "Failed to apply compact"))
                    .ok();
                    chat_view
                        .update(cx, |view, cx| view.add_info_message(msg, cx))
                        .map_err(|e| warn!(error = ?e, "Failed to show compact result"))
                        .ok();
                }
                Err(e) => {
                    let msg = format!("Failed to compact conversation: {e}");
                    chat_view
                        .update(cx, |view, cx| view.add_info_message(msg, cx))
                        .map_err(|e| warn!(error = ?e, "Failed to show compact error"))
                        .ok();
                }
            },
        )
        .detach();
    }

    /// `/context` — show token-usage statistics in the chat.
    fn show_context_info(&mut self, cx: &mut Context<Self>) {
        let snapshot = cx
            .try_global::<GlobalTokenBudget>()
            .and_then(|budget| budget.receiver.borrow().clone());

        let cwd = cx
            .try_global::<ExecutionSettingsModel>()
            .and_then(|s| s.workspace_dir.clone())
            .unwrap_or_else(|| {
                std::env::current_dir()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|_| ".".to_string())
            });

        let msg = if let Some(snap) = snapshot {
            let used = snap.estimated_total();
            let max = snap.model_context_limit;
            let pct = if max > 0 {
                (used as f64 / max as f64 * 100.0).clamp(0.0, 100.0)
            } else {
                0.0
            };
            let filled = ((pct / 100.0) * 20.0).round() as usize;
            let bar = format!(
                "[{}{}]",
                "█".repeat(filled.min(20)),
                "░".repeat(20usize.saturating_sub(filled.min(20)))
            );
            format!(
                "**Context usage:** {used} / {max} tokens ({pct:.1}%) {bar}\n\
                 **Working directory:** {cwd}"
            )
        } else {
            format!("**Context:** No snapshot available yet.\n**Working directory:** {cwd}")
        };

        self.chat_view.update(cx, |view, cx| {
            view.add_info_message(msg, cx);
        });
    }

    /// `/copy` — copy the last assistant response to the system clipboard.
    fn copy_last_response(&mut self, cx: &mut Context<Self>) {
        // Walk chat_view messages in reverse to find the last non-empty assistant message.
        let last_text = self
            .chat_view
            .read(cx)
            .messages()
            .iter()
            .rev()
            .find(|m| {
                matches!(
                    m.role,
                    crate::chatty::views::message_component::MessageRole::Assistant
                ) && !m.content.trim().is_empty()
                    && !m.is_streaming
            })
            .map(|m| m.content.clone());

        match last_text {
            Some(text) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                self.chat_view.update(cx, |view, cx| {
                    view.add_info_message(
                        "Copied latest assistant response to clipboard.".to_string(),
                        cx,
                    );
                });
            }
            None => {
                self.chat_view.update(cx, |view, cx| {
                    view.add_info_message(
                        "No assistant response available to copy.".to_string(),
                        cx,
                    );
                });
            }
        }
    }

    /// `/cwd` — show the current working directory.
    fn show_working_directory(&mut self, cx: &mut Context<Self>) {
        let cwd = cx
            .try_global::<ExecutionSettingsModel>()
            .and_then(|s| s.workspace_dir.clone())
            .unwrap_or_else(|| {
                std::env::current_dir()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|_| ".".to_string())
            });

        self.chat_view.update(cx, |view, cx| {
            view.add_info_message(format!("**Working directory:** {cwd}"), cx);
        });
    }

    // -----------------------------------------------------------------------
    // Arg-based slash command dispatch (called from ChatInputEvent::Send)
    // -----------------------------------------------------------------------

    /// Returns `true` when the message was handled as an arg-based slash command
    /// (so the caller should NOT forward it to the LLM).
    pub(super) fn try_handle_arg_slash_command(
        &mut self,
        text: &str,
        cx: &mut Context<Self>,
    ) -> bool {
        if let Some(rest) = text.strip_prefix("/agent ") {
            let rest = rest.trim().to_string();
            if rest.is_empty() {
                self.chat_view.update(cx, |view, cx| {
                    view.add_info_message(
                        "Usage: `/agent <prompt>` or `/agent <name> <prompt>` — \
                         dispatch to a local sub-agent or a registered A2A agent."
                            .to_string(),
                        cx,
                    );
                });
            } else {
                // Check if the first word matches a registered A2A agent name.
                let (agent_name, prompt_for_agent) = {
                    let mut words = rest.splitn(2, char::is_whitespace);
                    let first = words.next().unwrap_or("").to_string();
                    let tail = words.next().unwrap_or("").trim().to_string();
                    (first, tail)
                };

                let is_a2a_agent = !prompt_for_agent.is_empty()
                    && cx
                        .try_global::<chatty_core::settings::models::extensions_store::ExtensionsModel>()
                        .and_then(|m| m.find_enabled_a2a(&agent_name).map(|_| true))
                        .unwrap_or(false);

                if is_a2a_agent {
                    self.launch_a2a_agent(agent_name, prompt_for_agent, cx);
                } else {
                    // Fall back to local sub-agent with the entire rest as the prompt.
                    self.launch_agent(rest, cx);
                }
            }
            return true;
        }
        if let Some(path) = text.strip_prefix("/cd ") {
            let path = path.trim().to_string();
            if path.is_empty() {
                self.show_working_directory(cx);
            } else {
                self.change_working_dir(path, cx);
            }
            return true;
        }
        if let Some(path) = text.strip_prefix("/add-dir ") {
            let path = path.trim().to_string();
            if !path.is_empty() {
                self.add_directory(path, cx);
            }
            return true;
        }
        false
    }

    /// Dispatch a task to a remote A2A agent and display the result.
    fn launch_a2a_agent(&mut self, agent_name: String, prompt: String, cx: &mut Context<Self>) {
        info!(agent = %agent_name, prompt = %prompt, "Dispatching task to remote A2A agent");

        // Capture the config snapshot now (before the async spawn).
        let config = cx
            .try_global::<chatty_core::settings::models::extensions_store::ExtensionsModel>()
            .and_then(|m| m.find_enabled_a2a(&agent_name).cloned());

        let Some(config) = config else {
            self.chat_view.update(cx, |view, cx| {
                view.add_info_message(
                    format!("A2A agent \u{2018}{agent_name}\u{2019} not found or not enabled."),
                    cx,
                );
            });
            return;
        };

        // Capture conversation ID so we can inject the result even if the user
        // navigates away while the remote call is in flight.
        let launch_conv_id = cx
            .try_global::<ConversationsStore>()
            .and_then(|store| store.active_id().cloned());

        // Show immediate progress feedback.
        let prompt_for_display = prompt.clone();
        self.chat_view.update(cx, |view, cx| {
            let source = classify_agent_source(&agent_name, cx);
            view.start_sub_agent_progress(
                &format!("[Agent: {agent_name}] {prompt_for_display}"),
                source,
                cx,
            );
        });

        let chat_view = self.chat_view.clone();
        let prompt_label = prompt.clone();

        cx.spawn(async move |weak, cx| {
            use futures::StreamExt;

            let client = chatty_core::services::A2aClient::new();

            // Use streaming to match invoke_agent's visual behaviour.
            let stream_result = client.send_message_stream(&config, &prompt).await;

            let (success, result_text) =
                match stream_result {
                    Ok(mut stream) => {
                        let mut response = String::new();
                        let mut success = true;

                        while let Some(event) = stream.next().await {
                            match event {
                            Ok(chatty_core::services::a2a_client::A2aStreamEvent::StatusUpdate {
                                state,
                                message,
                                ..
                            }) => {
                                if state == "failed" {
                                    success = false;
                                    if let Some(msg) = message {
                                        response = format!("\u{26a0}\u{fe0f} {msg}");
                                    }
                                } else if state == "working"
                                    && let Some(ref msg) = message {
                                        chat_view
                                            .update(cx, |view, cx| {
                                                view.append_sub_agent_progress(msg, cx);
                                            })
                                            .ok();
                                    }
                            }
                            Ok(chatty_core::services::a2a_client::A2aStreamEvent::ArtifactUpdate {
                                text,
                                ..
                            }) => {
                                response.push_str(&text);
                            }
                            Err(e) => {
                                success = false;
                                response = format!("\u{26a0}\u{fe0f} A2A error: {e:#}");
                                break;
                            }
                        }
                        }

                        let result_text = if response.is_empty() {
                            None
                        } else {
                            Some(response)
                        };
                        (success, result_text)
                    }
                    Err(e) => (false, Some(format!("\u{26a0}\u{fe0f} A2A error: {e:#}"))),
                };

            // Inject into conversation history.
            if let (Some(conv_id), Some(txt)) = (&launch_conv_id, &result_text) {
                let user_entry = rig::completion::Message::User {
                    content: rig::OneOrMany::one(rig::message::UserContent::text(format!(
                        "[A2A task \u{2192} {agent_name}: {prompt_label}]"
                    ))),
                };
                let result_entry = format!("[A2A result from {agent_name}]\n\n{txt}");
                cx.update(|cx| {
                    cx.update_global::<ConversationsStore, _>(|store, _cx| {
                        if let Some(conv) = store.get_conversation_mut(conv_id) {
                            conv.add_user_message_with_attachments(user_entry, vec![]);
                            conv.finalize_response(result_entry, vec![], None);
                        }
                    });
                })
                .map_err(|e| warn!(error = ?e, "Failed to inject A2A result into conversation"))
                .ok();

                if let Some(app) = weak.upgrade() {
                    let conv_id_for_persist = conv_id.clone();
                    app.update(cx, |app, cx| {
                        app.persist_conversation(&conv_id_for_persist, cx);
                    })
                    .ok();
                }
            }

            // Finalize the progress trace.
            chat_view
                .update(cx, |view, cx| {
                    view.finalize_sub_agent_progress(success, result_text, cx)
                })
                .ok();
        })
        .detach();
    }

    /// `/agent <prompt>` — launch chatty-tui in headless mode with the given prompt.
    fn launch_agent(&mut self, prompt: String, cx: &mut Context<Self>) {
        info!(prompt = %prompt, "Slash command: launch sub-agent");

        // Capture the conversation where the sub-agent is launched so the result can be
        // routed back to the correct conversation even if the user navigates away.
        let launch_conv_id = cx
            .try_global::<ConversationsStore>()
            .and_then(|store| store.active_id().cloned());

        // If there is no conversation yet, create one first so that:
        // 1. The sub-agent result can be injected into history
        // 2. Sending a new message while the sub-agent runs won't trigger
        //    conversation creation (which calls clear_messages and would
        //    destroy the sub-agent progress trace)
        if launch_conv_id.is_none() {
            let prompt_clone = prompt.clone();
            let create_task = self.create_new_conversation(cx);
            let app_entity = cx.entity();
            cx.spawn(async move |_weak, cx| {
                match create_task.await {
                    Ok(_id) => {
                        app_entity
                            .update(cx, |app, cx| {
                                app.launch_agent(prompt_clone, cx);
                            })
                            .map_err(|e| warn!(error = ?e, "Failed to launch sub-agent after conversation creation"))
                            .ok();
                    }
                    Err(e) => {
                        error!(error = ?e, "Failed to create conversation for sub-agent");
                    }
                }
            })
            .detach();
            return;
        }

        // Resolve the active model ID so the sub-agent uses the same model.
        let model_id = cx
            .try_global::<ConversationsStore>()
            .and_then(|store| {
                store
                    .active_id()
                    .and_then(|id| store.get_conversation(id))
                    .map(|conv| conv.model_id().to_string())
            })
            .unwrap_or_default();

        let chat_view = self.chat_view.clone();

        // Show immediate feedback and record the message index for live progress.
        // Clone the prompt for the display before it is moved into the async task.
        let prompt_for_display = prompt.clone();
        self.chat_view.update(cx, |view, cx| {
            view.start_sub_agent_progress(&prompt_for_display, ToolSource::Local, cx);
        });

        // Channel for streaming stderr progress lines from the subprocess.
        let (progress_tx, mut progress_rx) = tokio::sync::mpsc::unbounded_channel::<String>();

        // Keep a copy of prompt to use in the history injection label below.
        let prompt_label = prompt.clone();

        cx.spawn(async move |weak, cx| {
            let mut blocking_fut = tokio::task::spawn_blocking(move || {
                use std::io::BufRead as _;
                use std::process::Stdio;

                // Look for chatty-tui in the same directory as this binary first,
                // then fall back to letting the OS resolve from PATH.
                let exe = std::env::current_exe()
                    .ok()
                    .and_then(|p| p.parent().map(|d| d.join("chatty-tui")))
                    .filter(|p| p.exists())
                    .unwrap_or_else(|| std::path::PathBuf::from("chatty-tui"));

                let mut cmd = std::process::Command::new(&exe);
                cmd.arg("--headless")
                    .arg("--message")
                    .arg(&prompt)
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped());
                if !model_id.is_empty() {
                    cmd.arg("--model").arg(&model_id);
                }
                // Headless sub-agents always run with auto-approve: there is no UI
                // available to show approval prompts, so without this flag any tool
                // that requires approval will block indefinitely and never complete.
                info!(exe = ?exe, "Launching headless sub-agent with auto-approve (no approval UI available)");
                cmd.arg("--auto-approve");

                let mut child = match cmd.spawn() {
                    Ok(c) => c,
                    Err(e) => return Err(format!("Sub-agent failed to launch: {e}")),
                };

                // Drain stderr in a background thread, forwarding each line as a
                // progress event so the parent TUI can show live tool-call activity.
                let stderr = child.stderr.take();
                let stderr_thread = std::thread::spawn(move || {
                    if let Some(stderr) = stderr {
                        let reader = std::io::BufReader::new(stderr);
                        for line in reader.lines().map_while(Result::ok) {
                            let _ = progress_tx.send(line);
                        }
                    }
                });

                let output = match child.wait_with_output() {
                    Ok(o) => o,
                    Err(e) => {
                        let _ = stderr_thread.join();
                        return Err(format!("Sub-agent failed: {e}"));
                    }
                };
                let _ = stderr_thread.join();

                if output.status.success() {
                    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
                    Ok(stdout)
                } else {
                    Err(format!(
                        "Sub-agent failed (exit {:?})",
                        output.status.code()
                    ))
                }
            });

            // Drive progress updates while the subprocess runs.
            // `biased` makes tokio::select! poll branches in declaration order, so
            // the progress-recv branch is always checked before the completion branch.
            // This guarantees that any lines already buffered in the channel are
            // delivered to the UI before we process the final result.
            let result = loop {
                tokio::select! {
                    biased;
                    Some(line) = progress_rx.recv() => {
                        chat_view
                            .update(cx, |view, cx| view.append_sub_agent_progress(&line, cx))
                            .ok();
                    }
                    result = &mut blocking_fut => {
                        // Drain any progress lines that arrived concurrently with
                        // the task completing.
                        while let Ok(line) = progress_rx.try_recv() {
                            chat_view
                                .update(cx, |view, cx| view.append_sub_agent_progress(&line, cx))
                                .ok();
                        }
                        break result;
                    }
                }
            };

            let agent_result: Result<String, String> = match result {
                Ok(r) => r,
                Err(e) => Err(format!("Sub-agent task panicked: {e}")),
            };

            let success = agent_result.is_ok();
            let result_text = match agent_result {
                Ok(stdout) if stdout.is_empty() => None,
                Ok(stdout) => Some(stdout),
                Err(e) => Some(format!("⚠️ {e}")),
            };

            // Inject the sub-agent result into the conversation history so the main
            // agent can reference it on subsequent turns.  We inject a User→Assistant
            // message pair to maintain the alternating role pattern that LLM APIs
            // expect.  The User message describes the task that was delegated and the
            // Assistant message contains the sub-agent's output.
            if let (Some(conv_id), Some(txt)) = (&launch_conv_id, &result_text) {
                let user_entry = rig::completion::Message::User {
                    content: rig::OneOrMany::one(rig::message::UserContent::text(format!(
                        "[Sub-agent task: {prompt_label}]",
                    ))),
                };
                cx.update(|cx| {
                    cx.update_global::<ConversationsStore, _>(|store, _cx| {
                        if let Some(conv) = store.get_conversation_mut(conv_id) {
                            conv.add_user_message_with_attachments(user_entry, vec![]);
                            conv.finalize_response(
                                format!("[Sub-agent result]\n\n{txt}"),
                                vec![],
                                None,
                            );
                        }
                    });
                })
                .map_err(|e| warn!(error = ?e, "Failed to inject sub-agent result into conversation history"))
                .ok();

                // Persist the updated history to disk.
                if let Some(app) = weak.upgrade() {
                    let conv_id_for_persist = conv_id.clone();
                    app.update(cx, |app, cx| {
                        app.persist_conversation(&conv_id_for_persist, cx);
                    })
                    .ok();
                }
            }

            // Finalize the collapsible trace — result goes inside the expanded body.
            // If the conversation changed, we still finalize so the trace is frozen,
            // but we also navigate back / show a fallback note below.
            let result_for_fallback = result_text.clone();
            chat_view
                .update(cx, |view, cx| {
                    view.finalize_sub_agent_progress(success, result_text, cx)
                })
                .ok();

            // If the user navigated to a different conversation while the sub-agent was
            // running, route the result back to the conversation where it was launched.
            // Exception: if the current conversation has an active LLM stream, avoid
            // disruptive navigation — show the result in the current view with a note.
            if let Some(ref conv_id) = launch_conv_id {
                let (current_conv_id, current_has_active_stream) = cx
                    .update(|cx| {
                        let current = cx
                            .try_global::<ConversationsStore>()
                            .and_then(|store| store.active_id().cloned());
                        let streaming = current
                            .as_ref()
                            .and_then(|id| {
                                cx.try_global::<GlobalStreamManager>()
                                    .and_then(|g| g.get())
                                    .map(|mgr| mgr.read(cx).is_streaming(id))
                            })
                            .unwrap_or(false);
                        (current, streaming)
                    })
                    .unwrap_or((None, false));

                let conversation_changed = current_conv_id.as_deref() != Some(conv_id.as_str());

                if conversation_changed {
                    if current_has_active_stream {
                        // A stream is active in the current conversation; navigating away
                        // would be disruptive. Show the result here with a context note.
                        if let Some(txt) = result_for_fallback {
                            let noted_msg =
                                format!("**Sub-agent** *(background task)*:\n\n{txt}");
                            chat_view
                                .update(cx, |view, cx| view.add_info_message(noted_msg, cx))
                                .map_err(|e| warn!(error = ?e, "Failed to show sub-agent result"))
                                .ok();
                        }
                        return;
                    }
                    // Navigate back to the launch conversation so the trace appears there.
                    let nav_conv_id = conv_id.clone();
                    if let Some(app) = weak.upgrade() {
                        app.update(cx, |app, cx| {
                            app.load_conversation(&nav_conv_id, cx);
                        })
                        .ok();
                    }
                }
            }
        })
        .detach();
    }

    /// `/cd <path>` — change the working directory stored in `ExecutionSettingsModel`.
    fn change_working_dir(&mut self, path: String, cx: &mut Context<Self>) {
        use std::path::Path;

        let resolved = {
            let base = cx
                .try_global::<ExecutionSettingsModel>()
                .and_then(|s| s.workspace_dir.clone())
                .map(std::path::PathBuf::from)
                .or_else(|| std::env::current_dir().ok())
                .unwrap_or_else(|| std::path::PathBuf::from("."));
            let candidate = Path::new(&path);
            if candidate.is_absolute() {
                candidate.to_path_buf()
            } else {
                base.join(candidate)
            }
        };

        match std::fs::canonicalize(&resolved) {
            Ok(canonical) if canonical.is_dir() => {
                let new_dir = canonical.to_string_lossy().to_string();
                cx.update_global::<ExecutionSettingsModel, _>(|s, _| {
                    s.workspace_dir = Some(new_dir.clone());
                });
                self.chat_view.update(cx, |view, cx| {
                    view.add_info_message(
                        format!("**Working directory changed to:** {new_dir}"),
                        cx,
                    );
                });
            }
            Ok(_) => {
                self.chat_view.update(cx, |view, cx| {
                    view.add_info_message(format!("`{path}` is not a directory."), cx);
                });
            }
            Err(e) => {
                self.chat_view.update(cx, |view, cx| {
                    view.add_info_message(format!("Cannot change directory to `{path}`: {e}"), cx);
                });
            }
        }
    }

    /// `/add-dir <path>` — validate and register a directory in the workspace.
    ///
    /// If `ExecutionSettingsModel.workspace_dir` is not yet set, this path becomes
    /// the workspace root.  Shows confirmation or an error message in the chat.
    fn add_directory(&mut self, path: String, cx: &mut Context<Self>) {
        use std::path::Path;

        let resolved = {
            let base = cx
                .try_global::<ExecutionSettingsModel>()
                .and_then(|s| s.workspace_dir.clone())
                .map(std::path::PathBuf::from)
                .or_else(|| std::env::current_dir().ok())
                .unwrap_or_else(|| std::path::PathBuf::from("."));
            let candidate = Path::new(&path);
            if candidate.is_absolute() {
                candidate.to_path_buf()
            } else {
                base.join(candidate)
            }
        };

        match std::fs::canonicalize(&resolved) {
            Ok(canonical) if canonical.is_dir() => {
                let dir = canonical.to_string_lossy().to_string();
                // If no workspace_dir is set yet, use the provided path as workspace root.
                cx.update_global::<ExecutionSettingsModel, _>(|s, _| {
                    if s.workspace_dir.is_none() {
                        s.workspace_dir = Some(dir.clone());
                    }
                });
                self.chat_view.update(cx, |view, cx| {
                    view.add_info_message(format!("**Directory added to context:** {dir}"), cx);
                });
            }
            Ok(_) => {
                self.chat_view.update(cx, |view, cx| {
                    view.add_info_message(format!("`{path}` is not a directory."), cx);
                });
            }
            Err(e) => {
                self.chat_view.update(cx, |view, cx| {
                    view.add_info_message(format!("Cannot add directory `{path}`: {e}"), cx);
                });
            }
        }
    }
}
