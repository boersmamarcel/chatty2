use super::*;
use chatty_core::agent_spec::AgentSpec;
use chatty_core::services::agent_command::{
    AgentCommandTarget, preflight, resolve_agent_command, workspace_line,
};
use chatty_core::session::Delegation;
use chatty_core::settings::models::a2a_store::A2aAgentConfig;
use chatty_core::tools::LOCAL_AGENT_NAME;

/// The local roster's specs, from `workspace` (PL-U5): what module settings
/// declare, else every exposed spec. A declared spec that does not load
/// leaves it empty, so `/agent` falls through to the default sub-agent.
///
/// `workspace` is the same effective workspace `list_agents`/`invoke_agent`
/// resolve for this conversation ([`workspace_for_active_conversation`]),
/// so `/agent <name>` can never resolve a name those tools would refuse
/// (AGE-719 — docs/agents-and-specs.md says all three read the roster the
/// same way).
fn local_roster(cx: &App, workspace: Option<&Path>) -> Vec<AgentSpec> {
    let declared = cx
        .try_global::<crate::settings::models::ModuleSettingsModel>()
        .map(|m| m.virtual_agents.clone())
        .unwrap_or_default();
    chatty_core::agent_spec::load_roster(&declared, workspace).unwrap_or_else(|error| {
        warn!(
            error = format!("{error:#}"),
            "The declared agent roster does not load"
        );
        Vec::new()
    })
}

/// The active conversation's own working directory when it has one, else
/// the shared default (AGE-719): the same resolution `gateway_and_roster`
/// uses to build a conversation's `local_agents`.
fn workspace_for_active_conversation(cx: &App) -> Option<PathBuf> {
    let default_workspace = cx
        .try_global::<ExecutionSettingsModel>()
        .and_then(|settings| settings.workspace_dir.clone());
    let conv_workspace = cx.try_global::<ConversationsStore>().and_then(|store| {
        store
            .active_id()
            .and_then(|id| store.get_conversation(id))
            .and_then(|conv| conv.working_dir())
            .cloned()
    });
    chatty_core::agent_spec::roster_workspace(
        default_workspace.as_deref().map(Path::new),
        conv_workspace.as_deref(),
    )
    .map(Path::to_path_buf)
}

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
                         dispatch to a sub-agent, an agent spec (Settings → Agents) \
                         or a registered A2A agent."
                            .to_string(),
                        cx,
                    );
                });
            } else {
                // The first word names a remote A2A agent or a spec on the
                // local roster — the one `list_agents` lists — or it is part
                // of the default sub-agent's prompt (PL-U5).
                let remote_agents: Vec<A2aAgentConfig> = cx
                    .try_global::<crate::settings::models::ExtensionsModel>()
                    .map(|m| {
                        m.all_a2a_agents()
                            .into_iter()
                            .map(|(_, config, enabled)| A2aAgentConfig { enabled, ..config })
                            .collect()
                    })
                    .unwrap_or_default();
                let workspace = workspace_for_active_conversation(cx);
                match resolve_agent_command(
                    &rest,
                    &remote_agents,
                    &local_roster(cx, workspace.as_deref()),
                ) {
                    AgentCommandTarget::Remote { config, prompt } => {
                        self.launch_a2a_agent(config.name, prompt, cx)
                    }
                    // A spec on the roster, or the default sub-agent by its
                    // roster name: both through the conversation's own
                    // broker, so the swarm tree shows (AGE-744) — once the
                    // workspace and code execution are there (AGE-822).
                    AgentCommandTarget::Spec { spec, prompt } => self.send_local_delegation(
                        Delegation {
                            agent: spec.agent.name,
                            prompt,
                        },
                        workspace.as_deref(),
                        cx,
                    ),
                    AgentCommandTarget::Default { prompt } => self.send_local_delegation(
                        Delegation {
                            agent: LOCAL_AGENT_NAME.to_string(),
                            prompt,
                        },
                        workspace.as_deref(),
                        cx,
                    ),
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

    /// Hand `delegation` to a local agent, or tell the user why not before
    /// anything is spawned: no workspace, or code execution off (AGE-822).
    /// The turn shows the workspace its agents work in under the command.
    fn send_local_delegation(
        &mut self,
        delegation: Delegation,
        workspace: Option<&Path>,
        cx: &mut Context<Self>,
    ) {
        let code_execution = cx
            .try_global::<ExecutionSettingsModel>()
            .is_some_and(|settings| settings.enabled);
        match preflight(&delegation.agent, workspace, code_execution) {
            Ok(workspace) => self.send_delegation(delegation, Some(workspace_line(&workspace)), cx),
            Err(card) => {
                warn!(agent = %delegation.agent, "/agent refused before spawning: {card}");
                self.chat_view
                    .update(cx, |view, cx| view.add_info_message(card, cx));
            }
        }
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
            view.start_delegation_progress(
                &format!("[Agent: {agent_name}] {prompt_for_display}"),
                source,
                cx,
            );
        });

        let chat_view = self.chat_view.clone();
        let prompt_label = prompt.clone();

        cx.spawn(async move |weak, cx| {
            use futures::StreamExt;

            // Streaming, to match invoke_agent's visual behaviour — and on
            // the delegation client, because a remote agent's answer takes as
            // long as it takes (AGE-319).
            let client = chatty_core::services::A2aClient::for_delegation();
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
                                                view.append_delegation_progress(msg, cx);
                                            })
                                            .map_err(|e| warn!(error = ?e, "Failed to update chat view with A2A progress"))
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
                let user_entry = rig_core::completion::Message::User {
                    content: vec![rig_core::message::UserContent::text(format!(
                        "[A2A task \u{2192} {agent_name}: {prompt_label}]"
                    ))],
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
                    .map_err(|e| warn!(error = ?e, "Failed to persist conversation after A2A result"))
                    .ok();
                }
            }

            // Finalize the progress trace.
            chat_view
                .update(cx, |view, cx| {
                    view.finalize_delegation_progress(success, result_text, cx)
                })
                .map_err(|e| warn!(error = ?e, "Failed to finalize A2A progress in chat view"))
                .ok();
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
