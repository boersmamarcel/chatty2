//! Conversation-modification operations for `ChattyApp` — the second half
//! of `conversation_ops.rs`, split out to keep both files under ~1000 LOC.
//!
//! # What lives here
//!
//! - Navigating between conversations.
//! - Starting / deleting active conversations from keyboard shortcuts.
//! - Changing the active conversation's model or working directory at
//!   runtime (rebuilding the agent in-place).
//! - Persisting a single conversation to disk.
//!
//! # What does NOT live here
//!
//! - Conversation creation / loading / restore — `conversation_ops.rs`.
//! - The persistence layer itself — `chatty_core::repositories::conversation_*`.

use super::*;

impl ChattyApp {
    /// Navigate to the next or previous conversation in the sidebar list.
    /// `direction`: -1 for previous (up in sidebar), +1 for next (down in sidebar).
    /// The sidebar list is sorted by updated_at descending, so "up" means older
    /// and "down" means newer relative to the current position.
    pub fn navigate_conversation(&mut self, direction: i32, cx: &mut Context<Self>) {
        let store = cx.global::<ConversationsStore>();
        let current_id = store.active_id().cloned();
        let conv_ids = store.all_metadata_ids();

        if conv_ids.is_empty() {
            return;
        }

        let target_id = if let Some(ref current) = current_id {
            if let Some(pos) = conv_ids.iter().position(|id| id == current) {
                let new_pos = if direction < 0 {
                    // Up in sidebar = previous (lower index wraps to end)
                    if pos == 0 {
                        conv_ids.len() - 1
                    } else {
                        pos - 1
                    }
                } else {
                    // Down in sidebar = next (higher index wraps to start)
                    if pos + 1 >= conv_ids.len() {
                        0
                    } else {
                        pos + 1
                    }
                };
                conv_ids[new_pos].clone()
            } else {
                // Active conversation not found in list, go to first
                conv_ids[0].clone()
            }
        } else {
            // No active conversation, go to first
            conv_ids[0].clone()
        };

        // Only switch if we're actually changing conversations
        if current_id.as_ref() != Some(&target_id) {
            self.load_conversation(&target_id, cx);
        }
    }

    /// Delete the currently active conversation.
    /// Start a new conversation, guarding against duplicate requests and cancelling
    /// any pending stream. Used by both the sidebar button and the keyboard shortcut.
    pub fn start_new_conversation(&mut self, cx: &mut Context<Self>) {
        if self.active_create_task.is_some() {
            debug!("Already creating a conversation, ignoring duplicate request");
            return;
        }
        // Cancel any pending stream before creating a new conversation
        if let Some(manager) = cx.try_global::<GlobalStreamManager>().and_then(|g| g.get()) {
            manager.update(cx, |mgr, cx| {
                mgr.cancel_pending(cx);
            });
        }
        let create_task = self.create_new_conversation(cx);
        self.active_create_task = Some(cx.spawn(async move |weak, cx| {
            let result = create_task.await;
            if let Some(app) = weak.upgrade() {
                app.update(cx, |app, _cx| app.active_create_task = None)
                    .map_err(|e| debug!(error = ?e, "Failed to clear active_create_task"))
                    .ok();
            }
            result
        }));
    }

    pub fn delete_active_conversation(&mut self, cx: &mut Context<Self>) {
        let active_id = cx.global::<ConversationsStore>().active_id().cloned();

        if let Some(id) = active_id {
            self.delete_conversation(&id, cx);
        }
    }

    /// Change the model for the active conversation
    /// Rebuild the active conversation's agent with fresh MCP tools, keeping the same model.
    /// Called after an MCP server is enabled or disabled so the agent's tool set stays current.
    pub(super) fn rebuild_active_agent(&mut self, cx: &mut Context<Self>) {
        let conv_id = cx
            .global::<ConversationsStore>()
            .active_id()
            .map(|s| s.to_string());

        let Some(conv_id) = conv_id else { return };

        cx.spawn(async move |_weak, cx| -> anyhow::Result<()> {
            rebuild_conversation_agent(&conv_id, cx).await
        })
        .detach();
    }

    pub(super) fn change_conversation_model(&mut self, model_id: String, cx: &mut Context<Self>) {
        debug!(model_id = %model_id, "Changing to model");

        // Get the active conversation ID
        let conv_id = cx
            .global::<ConversationsStore>()
            .active_id()
            .map(|s| s.to_string());

        if let Some(conv_id) = conv_id {
            // Get model and provider configs
            let models = cx.global::<ModelsModel>();
            let providers = cx.global::<ProviderModel>();

            if let Some(model_config) = models.get_model(&model_id) {
                if let Some(provider_config) = providers
                    .providers()
                    .iter()
                    .find(|p| p.provider_type == model_config.provider_type)
                {
                    let model_config = model_config.clone();
                    let provider_config = provider_config.clone();
                    let repo = self.conversation_repo.clone();

                    debug!("Found model and provider config");

                    // Update the conversation model
                    cx.spawn(async move |_weak, cx| -> anyhow::Result<()> {
                        // Get MCP service
                        let mcp_service = cx
                            .update(|cx| cx.global::<crate::chatty::services::McpService>().clone())
                            .map_err(|e| anyhow::anyhow!(e.to_string()))?;

                        // Get MCP tools from active servers
                        let mcp_tools =
                            chatty_core::services::gather_mcp_tools(&mcp_service).await;

                        debug!(
                            has_mcp_tools = mcp_tools.is_some(),
                            "Creating agent with MCP tools"
                        );

                        // Get execution settings for tool creation
                        let (
                            exec_settings,
                            pending_approvals,
                            pending_write_approvals,
                            pending_artifacts,
                            shell_session,
                            user_secrets,
                            theme_colors,
                            search_settings,
                            built_workspace_dir,
                        ) = cx
                            .update(|cx| {
                                let mut settings = cx
                                    .global::<crate::settings::models::ExecutionSettingsModel>()
                                    .clone();
                                let approvals = cx
                                    .global::<crate::chatty::models::ExecutionApprovalStore>()
                                    .get_pending_approvals();
                                let write_approvals = cx
                                    .global::<crate::chatty::models::WriteApprovalStore>()
                                    .get_pending_approvals();
                                let conv =
                                    cx.global::<ConversationsStore>().get_conversation(&conv_id);
                                if let Some(working_dir) = conv.and_then(|c| c.working_dir()) {
                                    settings.workspace_dir = Some(
                                        normalize_workspace_path(working_dir)
                                            .to_string_lossy()
                                            .to_string(),
                                    );
                                }
                                let built_workspace_dir = settings
                                    .workspace_dir
                                    .as_ref()
                                    .map(|dir| normalize_workspace_path(Path::new(dir)));
                                let artifacts = conv.map(|c| c.pending_artifacts());
                                let session = conv.and_then(|c| c.shell_session());
                                let secrets = cx
                                    .global::<crate::settings::models::UserSecretsModel>()
                                    .as_env_pairs();
                                let colors = extract_theme_chart_colors(cx);
                                let search_cfg = cx
                                    .try_global::<crate::settings::models::SearchSettingsModel>()
                                    .cloned();
                                (
                                    Some(settings),
                                    Some(approvals),
                                    Some(write_approvals),
                                    artifacts,
                                    session,
                                    secrets,
                                    Some(colors),
                                    search_cfg,
                                    built_workspace_dir,
                                )
                            })
                            .map_err(|e| anyhow::anyhow!(e.to_string()))?;

                        // Get memory service if available
                        // Wait for memory service init to complete before building the agent
                        let memory_service = await_memory_service(cx).await;
                        let embedding_service = get_embedding_service(cx);
                        let skill_service = get_skill_service(cx);
                        let module_agents = cx
                            .update(|cx| collect_module_agents(cx))
                            .unwrap_or_default();
                        let gateway_port = cx
                            .update(|cx| {
                                cx.try_global::<crate::settings::models::ModuleSettingsModel>()
                                    .map(|m| m.gateway_port)
                            })
                            .ok()
                            .flatten();
                        let (remote_agents, available_model_ids) = cx
                            .update(|cx| {
                                let agents = cx
                                    .try_global::<chatty_core::settings::models::extensions_store::ExtensionsModel>()
                                    .map(|m| m.a2a_agent_configs())
                                    .unwrap_or_default();
                                let model_ids = cx
                                    .try_global::<crate::settings::models::ModelsModel>()
                                    .map(|m| {
                                        m.models().iter().map(|m| m.id.clone()).collect::<Vec<_>>()
                                    })
                                    .unwrap_or_default();
                                (agents, model_ids)
                            })
                            .unwrap_or_default();

                        // Factory creates shell session on-demand if not provided
                        let (new_agent, new_shell_session, new_progress_slot) =
                            AgentClient::from_model_config_with_tools(
                                &model_config,
                                &provider_config,
                                AgentBuildContext {
                                    mcp_tools,
                                    exec_settings,
                                    pending_approvals,
                                    pending_write_approvals,
                                    pending_artifacts,
                                    shell_session,
                                    user_secrets,
                                    theme_colors,
                                    memory_service,
                            skill_service: Some(skill_service),
                                    search_settings,
                                    embedding_service,
                                    allow_sub_agent: true, // interactive agent: sub-agent tool is allowed
                                    module_agents,
                                    gateway_port,
                                    remote_agents,
                                    available_model_ids,
                                },
                            )
                            .await?;

                        // Update the conversation's agent synchronously
                        cx.update_global::<ConversationsStore, _>(|store, _cx| {
                            if let Some(conv) = store.get_conversation_mut(&conv_id) {
                                debug!("Updating conversation model");
                                conv.set_agent(
                                    new_agent,
                                    model_config.id.clone(),
                                    built_workspace_dir.clone(),
                                );
                                // Always store the new shell session — the factory either reused
                                // the existing one or created a fresh one.
                                if new_shell_session.is_some() {
                                    conv.set_shell_session(new_shell_session);
                                }
                                conv.set_invoke_agent_progress_slot(new_progress_slot);
                                Ok(())
                            } else {
                                Err(anyhow::anyhow!("Conversation not found"))
                            }
                        })
                        .map_err(|e| anyhow::anyhow!(e.to_string()))??;

                        debug!("Model updated successfully");

                        // Save to disk
                        let conv_data_res =
                            cx.update_global::<ConversationsStore, _>(|store, _cx| {
                                store.get_conversation(&conv_id).and_then(|conv| {
                                    let history = conv.serialize_history().ok()?;
                                    let traces = conv.serialize_traces().ok()?;
                                    let now = SystemTime::now()
                                        .duration_since(SystemTime::UNIX_EPOCH)
                                        .unwrap_or_default()
                                        .as_secs()
                                        as i64;

                                    Some(ConversationData {
                                        id: conv.id().to_string(),
                                        title: conv.title().to_string(),
                                        model_id: conv.model_id().to_string(),
                                        message_history: history,
                                        system_traces: traces,
                                        token_usage: conv
                                            .serialize_token_usage()
                                            .unwrap_or_else(|_| "{}".to_string()),
                                        attachment_paths: conv
                                            .serialize_attachment_paths()
                                            .unwrap_or_else(|_| "[]".to_string()),
                                        message_timestamps: conv
                                            .serialize_message_timestamps()
                                            .unwrap_or_else(|_| "[]".to_string()),
                                        message_feedback: conv
                                            .serialize_message_feedback()
                                            .unwrap_or_else(|_| "[]".to_string()),
                                        regeneration_records: conv
                                            .serialize_regeneration_records()
                                            .unwrap_or_else(|_| "[]".to_string()),
                                        created_at: conv
                                            .created_at()
                                            .duration_since(SystemTime::UNIX_EPOCH)
                                            .unwrap_or_default()
                                            .as_secs()
                                            as i64,
                                        updated_at: now,
                                        working_dir: conv
                                            .working_dir()
                                            .map(|p| p.to_string_lossy().to_string()),
                                    })
                                })
                            });

                        if let Ok(Some(conv_data)) = conv_data_res {
                            repo.save(&conv_id, conv_data)
                                .await
                                .map_err(|e| anyhow::anyhow!(e))?;
                            debug!("Conversation saved to disk");
                        }

                        Ok(())
                    })
                    .detach();
                } else {
                    error!("Provider not found");
                }
            } else {
                error!("Model not found");
            }
        }
    }

    /// Change the working directory for the active conversation
    pub(super) fn change_conversation_working_dir(
        &mut self,
        dir: Option<std::path::PathBuf>,
        cx: &mut Context<Self>,
    ) {
        let dir = dir.map(|path| normalize_workspace_path(&path));
        let conv_id = cx
            .global::<ConversationsStore>()
            .active_id()
            .map(|s| s.to_string());

        let Some(conv_id) = conv_id else { return };

        info!(conv_id = %conv_id, dir = ?dir, "Changing conversation working directory");

        // Update the working dir on the active conversation
        cx.update_global::<ConversationsStore, _>(|store, _cx| {
            if let Some(conv) = store.get_conversation_mut(&conv_id) {
                conv.set_working_dir(dir.clone());
            }
        });

        self.persist_conversation(&conv_id, cx);

        // Rebuild the agent so the new workspace_dir takes effect for tools and shell
        self.rebuild_active_agent(cx);

        // Refresh skills for the new working directory
        self.refresh_chat_input_skills(dir.as_deref(), cx);
    }

    /// Delete a conversation
    pub(super) fn delete_conversation(&mut self, id: &str, cx: &mut Context<Self>) {
        let conv_id = id.to_string();
        let repo = self.conversation_repo.clone();
        let sidebar = self.sidebar_view.clone();
        let chat_view = self.chat_view.clone();

        // Remove from global store
        cx.update_global::<ConversationsStore, _>(|store, _cx| {
            store.delete_conversation(&conv_id);
        });

        // Update sidebar
        sidebar.update(cx, |sidebar, cx| {
            let store = cx.global::<ConversationsStore>();
            let total = store.count();
            let convs = store.list_recent_metadata(sidebar.visible_limit());
            sidebar.set_total_count(total);

            let active_id = cx
                .global::<ConversationsStore>()
                .active_id()
                .map(|s| s.to_string());

            sidebar.set_conversations(convs, cx);
            sidebar.set_active_conversation(active_id.clone(), cx);
        });

        // If deleted conversation was active, clear chat view or load new active
        let active_id = cx
            .global::<ConversationsStore>()
            .active_id()
            .map(|s| s.to_string());
        if let Some(id) = active_id {
            self.load_conversation(&id, cx);
        } else {
            chat_view.update(cx, |view, cx| {
                view.clear_messages(cx);
                view.set_conversation_id(String::new(), cx);
            });
        }

        // Delete from disk
        cx.spawn(async move |_weak, _cx| {
            if let Err(e) = repo.delete(&conv_id).await {
                warn!(error = ?e, conv_id = %conv_id, "Failed to delete conversation from disk");
            }
            Ok::<_, anyhow::Error>(())
        })
        .detach();
    }

    /// Persist a conversation to disk asynchronously.
    /// Also updates the metadata store so the sidebar reflects the latest title and cost.
    pub(super) fn persist_conversation(&self, conv_id: &str, cx: &mut Context<Self>) {
        let conv_id = conv_id.to_string();
        let repo = self.conversation_repo.clone();

        let conv_data_opt = cx.update_global::<ConversationsStore, _>(|store, _cx| {
            store
                .get_conversation(&conv_id)
                .and_then(build_conversation_data)
        });

        if let Some(conv_data) = conv_data_opt {
            // Update metadata so title and cost changes are reflected in the sidebar
            let total_cost = conv_data.total_cost();
            cx.update_global::<ConversationsStore, _>(|store, _| {
                store.upsert_metadata(
                    &conv_data.id,
                    &conv_data.title,
                    total_cost,
                    conv_data.updated_at,
                );
            });

            debug!(
                conv_id = %conv_id,
                traces_json_len = conv_data.system_traces.len(),
                history_json_len = conv_data.message_history.len(),
                "Persisting conversation data"
            );

            let conv_id_for_save = conv_id.clone();
            cx.spawn(async move |_, _cx| {
                if let Err(e) = repo.save(&conv_id_for_save, conv_data).await {
                    warn!(error = ?e, conv_id = %conv_id_for_save, "Failed to save conversation to disk");
                } else {
                    debug!(conv_id = %conv_id_for_save, "Conversation saved to disk");
                }
                Ok::<_, anyhow::Error>(())
            })
            .detach();
        } else {
            error!(conv_id = %conv_id, "Failed to build conversation data for persistence (serialization failed)");
        }
    }
}
