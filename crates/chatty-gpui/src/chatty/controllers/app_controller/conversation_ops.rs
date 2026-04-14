use super::*;

impl ChattyApp {
    /// Restore a single conversation from persisted data
    ///
    /// Looks up the model and provider configs, then calls Conversation::from_data()
    async fn restore_conversation_from_data(
        data: ConversationData,
        models: &ModelsModel,
        providers: &ProviderModel,
        mcp_service: &crate::chatty::services::McpService,
        mut ctx: AgentBuildContext,
    ) -> anyhow::Result<Conversation> {
        if let Some(working_dir) = data.working_dir.as_ref()
            && let Some(ref mut exec) = ctx.exec_settings
        {
            exec.workspace_dir = Some(normalize_workspace_string(working_dir));
        }

        // Look up model config by ID
        let model_config = models.get_model(&data.model_id).ok_or_else(|| {
            anyhow::anyhow!(
                "Model '{}' not found (may have been deleted)",
                data.model_id
            )
        })?;

        // Find matching provider
        let provider_config = providers
            .providers()
            .iter()
            .find(|p| p.provider_type == model_config.provider_type)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "No provider found for model type {:?}",
                    model_config.provider_type
                )
            })?;

        // Get MCP tools for restoring conversation.
        // NOTE: MCP tools are fetched once at conversation creation/restore time and baked
        // into the AgentClient. If an MCP server is added, removed, or restarted after this
        // point, the existing conversation will retain its original tool set. Open a new
        // conversation to pick up updated tool registrations.
        let mcp_tools = chatty_core::services::gather_mcp_tools(mcp_service).await;
        ctx.mcp_tools = mcp_tools;

        // Restore conversation using factory method (bash tool will be created in agent_factory if enabled)
        Conversation::from_data(data, model_config, provider_config, ctx).await
    }

    /// Load conversation metadata at startup (fast — no message deserialization).
    ///
    /// Only loads lightweight id/title/cost metadata for the sidebar. Full conversation
    /// data is loaded lazily when the user selects a conversation.
    pub(super) fn load_conversations(&self, cx: &mut Context<Self>) {
        let repo = self.conversation_repo.clone();
        let sidebar = self.sidebar_view.clone();
        let chat_view = self.chat_view.clone();

        cx.spawn(async move |weak, cx| {
            match repo.load_metadata().await {
                Ok(metadata) => {
                    let count = metadata.len();
                    info!(count = count, "Loaded conversation metadata");

                    // Store metadata in the global store
                    cx.update_global::<ConversationsStore, _>(|store, _| {
                        store.set_metadata(metadata);
                    })
                    .map_err(|e| debug!(error = ?e, "Failed to store metadata"))
                    .ok();

                    // Update sidebar immediately from metadata — no full conversation load needed
                    sidebar
                        .update(cx, |sidebar, cx| {
                            let store = cx.global::<ConversationsStore>();
                            let total = store.count();
                            let convs = store.list_recent_metadata(sidebar.visible_limit());
                            debug!(conv_count = convs.len(), total = total, "Metadata loaded, updating sidebar");
                            sidebar.set_conversations(convs, cx);
                            sidebar.set_total_count(total);
                            sidebar.set_active_conversation(None, cx);
                        })
                        .map_err(|e| debug!(error = ?e, "Failed to update sidebar after metadata load"))
                        .ok();

                    // Clear chat view so the first message creates a new conversation
                    chat_view
                        .update(cx, |view, cx| {
                            view.set_conversation_id(String::new(), cx);
                            view.clear_messages(cx);
                            cx.notify();
                        })
                        .map_err(|e| debug!(error = ?e, "Failed to clear chat view on startup"))
                        .ok();

                    if count == 0 {
                        // No conversations yet — create the first one
                        info!("No conversations on disk, creating initial conversation");
                        if let Some(app) = weak.upgrade() {
                            let task_result =
                                app.update(cx, |app, cx| app.create_new_conversation(cx));
                            if let Ok(task) = task_result {
                                let _ = task.await;
                            }
                            app.update(cx, |app, cx| {
                                app.is_ready = true;
                                info!("App is now ready (initial conversation created)");
                                cx.notify();
                            })
                            .map_err(|e| debug!(error = ?e, "Failed to mark app ready after initial conversation"))
                            .ok();
                        }
                    } else {
                        // Conversations exist — app is ready; full data loaded on demand
                        if let Some(app) = weak.upgrade() {
                            let _: Result<(), _> = app.update(cx, |app, cx| {
                                app.is_ready = true;
                                info!("App is now ready (metadata loaded, conversations loaded on demand)");
                                cx.notify();
                            });
                        }
                    }
                }
                Err(e) => {
                    error!(error = ?e, "Failed to load conversation metadata");
                    // Still create an initial conversation so the app is usable
                    if let Some(app) = weak.upgrade() {
                        let task_result =
                            app.update(cx, |app, cx| app.create_new_conversation(cx));
                        if let Ok(task) = task_result {
                            let _ = task.await;
                        }
                        app.update(cx, |app, cx| {
                            app.is_ready = true;
                            info!("App is now ready (started after metadata load error)");
                            cx.notify();
                        })
                        .map_err(|warn_e| debug!(error = ?warn_e, "Failed to mark app ready after load error"))
                        .ok();
                    }
                }
            }

            Ok::<_, anyhow::Error>(())
        })
        .detach();
    }

    /// Create a new conversation
    ///
    /// Provides immediate UI feedback (clears chat, updates sidebar) before
    /// performing the potentially slow async work (MCP tool fetching, agent
    /// creation). This prevents the button from appearing unresponsive.
    ///
    /// Phases:
    /// 1. Synchronous: generate ID, update sidebar + chat view immediately
    /// 2. Async: fetch MCP tools, create agent, build Conversation object
    /// 3. Async: add to ConversationsStore, persist to disk
    pub fn create_new_conversation(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Task<anyhow::Result<String>> {
        info!("Creating new conversation");

        // Use the selected model from chat input, falling back to first available
        let selected_model_id = self
            .chat_view
            .read(cx)
            .chat_input_state()
            .read(cx)
            .selected_model_id()
            .cloned();
        let selected_working_dir = self
            .chat_view
            .read(cx)
            .chat_input_state()
            .read(cx)
            .working_dir()
            .map(|path| normalize_workspace_path(path));

        let models = cx.global::<ModelsModel>();
        let providers = cx.global::<ProviderModel>();

        let model_config = selected_model_id
            .as_ref()
            .and_then(|id| models.get_model(id).cloned())
            .or_else(|| models.models().first().cloned());

        if let Some(model_config) = model_config {
            // Find the provider for this model
            if let Some(provider_config) = providers
                .providers()
                .iter()
                .find(|p| p.provider_type == model_config.provider_type)
            {
                let model_config = model_config.clone();
                let provider_config = provider_config.clone();
                let chat_view = self.chat_view.clone();
                let sidebar = self.sidebar_view.clone();
                let repo = self.conversation_repo.clone();

                // PHASE 1: Immediate UI feedback (synchronous, before any async work)
                // Generate the conversation ID and title now so we can update UI instantly
                let conv_id = uuid::Uuid::new_v4().to_string();
                let title = "New Chat".to_string();

                // Clear chat view immediately so the user sees a fresh state
                chat_view.update(cx, |view, cx| {
                    view.set_conversation_id(conv_id.clone(), cx);
                    view.clear_messages(cx);

                    // Set available models in chat input
                    let models_list: Vec<(String, String)> = cx
                        .global::<ModelsModel>()
                        .models()
                        .iter()
                        .map(|m| (m.id.clone(), m.name.clone()))
                        .collect();

                    view.chat_input_state().update(cx, |state, cx| {
                        state.set_available_models(models_list, Some(model_config.id.clone()));
                        state.set_capabilities(
                            model_config.supports_images,
                            model_config.supports_pdf,
                        );
                        // Reset streaming state for new conversation (Bug Fix #1)
                        state.set_streaming(false, cx);
                        // Clear input text field for new conversation (Bug Fix #3)
                        state.mark_for_clear();
                    });
                });

                // Update sidebar immediately with the new conversation entry (placeholder)
                // Also insert a metadata entry so the count and list are correct
                let now_ts = SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs() as i64;
                cx.update_global::<ConversationsStore, _>(|store, _| {
                    store.upsert_metadata(&conv_id, &title, 0.0, now_ts);
                    store.set_active_by_id(conv_id.clone());
                });
                sidebar.update(cx, |sidebar, cx| {
                    let store = cx.global::<ConversationsStore>();
                    let total = store.count();
                    let convs = store.list_recent_metadata(sidebar.visible_limit());
                    sidebar.set_total_count(total);
                    sidebar.set_conversations(convs, cx);
                    sidebar.set_active_conversation(Some(conv_id.clone()), cx);
                    debug!("Sidebar updated immediately with new conversation placeholder");
                });

                // PHASE 2: Async work — MCP tools, agent creation, store + persist
                cx.spawn(async move |_weak, cx| {
                    // Get MCP tools.
                    // NOTE: MCP tools are fetched once at conversation creation time and baked
                    // into the AgentClient. If an MCP server is added, removed, or restarted
                    // after this point, the existing conversation will retain its original tool
                    // set. Open a new conversation to pick up updated tool registrations.
                    let mcp_service = cx
                        .update_global::<crate::chatty::services::McpService, _>(|svc, _| {
                            svc.clone()
                        })
                        .map_err(|e| anyhow::anyhow!("Failed to get MCP service: {}", e))?;
                    let mcp_tools =
                        chatty_core::services::gather_mcp_tools(&mcp_service).await;

                    // Get execution settings, approval stores, user secrets, and theme colors for tools
                    let (
                        exec_settings,
                        pending_approvals,
                        pending_write_approvals,
                        user_secrets,
                        theme_colors,
                        search_settings,
                    ) = cx.update(|cx| {
                        let mut settings = cx
                            .global::<crate::settings::models::ExecutionSettingsModel>()
                            .clone();
                        if let Some(working_dir) = selected_working_dir.as_ref() {
                            settings.workspace_dir = Some(
                                normalize_workspace_path(working_dir)
                                    .to_string_lossy()
                                    .to_string(),
                            );
                        }
                        let approvals = cx
                            .global::<crate::chatty::models::ExecutionApprovalStore>()
                            .get_pending_approvals();
                        let write_approvals = cx
                            .global::<crate::chatty::models::WriteApprovalStore>()
                            .get_pending_approvals();
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
                            secrets,
                            Some(colors),
                            search_cfg,
                        )
                    })?;

                    // Wait for memory service init to complete before building the agent
                    let memory_service = await_memory_service(cx).await;
                    let embedding_service = get_embedding_service(cx);
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

                    let mut conversation = Conversation::new(
                        conv_id.clone(),
                        title.clone(),
                        &model_config,
                        &provider_config,
                        AgentBuildContext {
                            mcp_tools,
                            exec_settings,
                            pending_approvals,
                            pending_write_approvals,
                            pending_artifacts: None, // set inside Conversation::new
                            shell_session: None,
                            user_secrets,
                            theme_colors,
                            memory_service,
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
                    conversation.set_working_dir(selected_working_dir.clone());

                    // PHASE 3: Add to global store and refresh sidebar with real data
                    cx.update_global::<ConversationsStore, _>(|store, _cx| {
                        store.insert_loaded(conversation);
                        store.set_active_by_id(conv_id.clone());
                    })?;

                    // Refresh sidebar — metadata was already inserted in PHASE 1 placeholder
                    sidebar.update(cx, |sidebar, cx| {
                        let store = cx.global::<ConversationsStore>();
                        let total = store.count();
                        let convs = store.list_recent_metadata(sidebar.visible_limit());
                        sidebar.set_total_count(total);
                        debug!(
                            conv_count = convs.len(),
                            "Updating sidebar after conversation creation"
                        );
                        sidebar.set_conversations(convs, cx);
                        sidebar.set_active_conversation(Some(conv_id.clone()), cx);
                    })?;

                    // Save to disk
                    let now = SystemTime::now()
                        .duration_since(SystemTime::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs() as i64;

                    let data = ConversationData {
                        id: conv_id.clone(),
                        title,
                        model_id: model_config.id.clone(),
                        message_history: "[]".to_string(),
                        system_traces: "[]".to_string(),
                        token_usage: "{}".to_string(),
                        attachment_paths: "[]".to_string(),
                        message_timestamps: "[]".to_string(),
                        message_feedback: "[]".to_string(),
                        regeneration_records: "[]".to_string(),
                        created_at: now,
                        updated_at: now,
                        working_dir: selected_working_dir
                            .as_ref()
                            .map(|path| path.to_string_lossy().to_string()),
                    };

                    repo.save(&conv_id, data)
                        .await
                        .map_err(|e| anyhow::anyhow!(e))?;

                    Ok(conv_id)
                })
            } else {
                let err_msg = "No provider found for model";
                error!("{}", err_msg);
                Task::ready(Err(anyhow::anyhow!(err_msg)))
            }
        } else {
            let err_msg = "No models configured";
            error!("{}", err_msg);
            // TODO: Show error in UI
            Task::ready(Err(anyhow::anyhow!(err_msg)))
        }
    }

    /// Load a conversation by ID.
    ///
    /// Fast path: if the conversation is already in memory, display it immediately.
    /// Slow path: load full data from SQLite, restore it, then display.
    pub(super) fn load_conversation(&mut self, id: &str, cx: &mut Context<Self>) {
        let conv_id = id.to_string();
        let sidebar = self.sidebar_view.clone();

        // Clear stale token budget snapshot so the bar shows "no data" during the
        // conversation transition rather than flashing the previous conversation's numbers.
        if cx.has_global::<GlobalTokenBudget>() {
            cx.global::<GlobalTokenBudget>().clear();
        }

        // Update sidebar active state immediately
        sidebar.update(cx, |sidebar, cx| {
            sidebar.set_active_conversation(Some(conv_id.clone()), cx);
        });

        // Mark as active in the store regardless of whether it is loaded
        cx.update_global::<ConversationsStore, _>(|store, _| {
            store.set_active_by_id(conv_id.clone());
        });

        if cx.global::<ConversationsStore>().is_loaded(id) {
            // Fast path: full data already in memory
            self.display_loaded_conversation(id, cx);
        } else {
            // Slow path: fetch from SQLite, restore, then display
            let repo = self.conversation_repo.clone();
            let module_agents = collect_module_agents(cx);
            let gateway_port = cx
                .try_global::<crate::settings::models::ModuleSettingsModel>()
                .map(|m| m.gateway_port);
            let remote_agents = cx
                .try_global::<chatty_core::settings::models::extensions_store::ExtensionsModel>()
                .map(|m| m.a2a_agent_configs())
                .unwrap_or_default();
            let available_model_ids = cx
                .try_global::<crate::settings::models::ModelsModel>()
                .map(|m| m.models().iter().map(|m| m.id.clone()).collect::<Vec<_>>())
                .unwrap_or_default();
            cx.spawn(async move |weak, cx| {
                let models = cx.update_global::<ModelsModel, _>(|m, _| m.clone())?;
                let providers = cx.update_global::<ProviderModel, _>(|p, _| p.clone())?;
                let mcp_service = cx.update_global::<crate::chatty::services::McpService, _>(|s, _| s.clone())?;
                let exec_settings = cx.update_global::<crate::settings::models::ExecutionSettingsModel, _>(|s, _| s.clone())?;
                let pending_approvals = cx.update_global::<crate::chatty::models::ExecutionApprovalStore, _>(|s, _| s.get_pending_approvals())?;
                let pending_write_approvals = cx.update_global::<crate::chatty::models::WriteApprovalStore, _>(|s, _| s.get_pending_approvals())?;
                let user_secrets = cx.update_global::<crate::settings::models::UserSecretsModel, _>(|m, _| m.as_env_pairs()).unwrap_or_default();
                let theme_colors = cx.update(|cx| extract_theme_chart_colors(cx)).ok();

                let memory_service = await_memory_service(cx).await;
                let search_settings = cx.update(|cx| {
                    cx.try_global::<crate::settings::models::SearchSettingsModel>().cloned()
                }).ok().flatten();

                match repo.load_one(&conv_id).await {
                    Ok(Some(data)) => {
                        let embedding_service = get_embedding_service(cx);
                        match Self::restore_conversation_from_data(
                            data, &models, &providers, &mcp_service,
                            AgentBuildContext {
                                mcp_tools: None,
                                exec_settings: Some(exec_settings.clone()),
                                pending_approvals: Some(pending_approvals),
                                pending_write_approvals: Some(pending_write_approvals),
                                pending_artifacts: None,
                                shell_session: None,
                                user_secrets,
                                theme_colors,
                                memory_service,
                                search_settings,
                                embedding_service,
                                allow_sub_agent: true,
                                module_agents,
                                gateway_port,
                                remote_agents,
                                available_model_ids,
                            },
                        )
                        .await
                        {
                            Ok(conversation) => {
                                // Insert and check active state atomically to avoid a TOCTOU
                                // where the user switches conversations between the insert and check.
                                let is_still_active = cx
                                    .update_global::<ConversationsStore, _>(|store, _| {
                                        store.insert_loaded(conversation);
                                        store.active_id().map(|id| id == &conv_id).unwrap_or(false)
                                    })
                                    .unwrap_or(false);

                                if is_still_active
                                    && let Some(app) = weak.upgrade()
                                {
                                    app.update(cx, |app, cx| {
                                        app.display_loaded_conversation(&conv_id, cx);
                                    })
                                    .map_err(|e| warn!(error = ?e, "Failed to display lazy-loaded conversation"))
                                    .ok();
                                }
                            }
                            Err(e) => {
                                warn!(conv_id = %conv_id, error = ?e, "Failed to restore lazy-loaded conversation");
                            }
                        }
                    }
                    Ok(None) => {
                        warn!(conv_id = %conv_id, "Conversation not found in DB during lazy load");
                    }
                    Err(e) => {
                        warn!(conv_id = %conv_id, error = ?e, "Failed to load conversation from DB");
                    }
                }

                Ok::<_, anyhow::Error>(())
            })
            .detach();
        }
    }

    /// Display a conversation that is already loaded in the ConversationsStore.
    fn display_loaded_conversation(&mut self, id: &str, cx: &mut Context<Self>) {
        // Clear stale invoke_agent IDs from the previous conversation to
        // prevent suppressing ToolCallBlocks that happen to share an ID.
        self.active_invoke_agent_ids.clear();

        let conv_id = id.to_string();
        let chat_view = self.chat_view.clone();

        let minimal_data = cx
            .global::<ConversationsStore>()
            .get_conversation(id)
            .map(|conv| {
                (
                    conv.model_id().to_string(),
                    conv.streaming_message().cloned(),
                    conv.streaming_trace().cloned(),
                    conv.working_dir().cloned(),
                )
            });

        if let Some((model_id, streaming_content, streaming_trace, conversation_working_dir)) =
            minimal_data
        {
            // Check if this conversation has an active stream via StreamManager
            let has_active_stream = cx
                .try_global::<GlobalStreamManager>()
                .and_then(|g| g.get())
                .map(|mgr| mgr.read(cx).is_streaming(&conv_id))
                .unwrap_or(false);

            // Get model capabilities
            let model_capabilities = cx
                .global::<ModelsModel>()
                .get_model(&model_id)
                .map(|m| (m.supports_images, m.supports_pdf))
                .unwrap_or((false, false));

            chat_view.update(cx, |view, cx| {
                view.set_conversation_id(conv_id.clone(), cx);

                // Clear attachments from previous conversation
                view.chat_input_state().update(cx, |state, _cx| {
                    state.clear_attachments();
                });

                // Load conversation history
                let entries = cx.global::<ConversationsStore>()
                    .get_conversation(&conv_id)
                    .map(|conv| conv.entries().to_vec());

                if let Some(entries) = entries {
                    view.load_history(&entries, cx);
                }

                // Update the selected model and capabilities in the chat input
                view.chat_input_state().update(cx, |state, cx| {
                    state.set_selected_model_id(model_id);
                    state.set_capabilities(model_capabilities.0, model_capabilities.1);

                    // Restore streaming state if conversation has active stream
                    // Set this BEFORE restoring the message so the UI is in correct state
                    state.set_streaming(has_active_stream, cx);

                    // Restore the per-conversation working directory override without emitting
                    // a WorkingDirChanged event (which would trigger an unnecessary agent rebuild)
                    state.set_working_dir_silent(conversation_working_dir.clone());
                });

                // Restore in-progress streaming message from Conversation model if it exists
                // This must happen AFTER setting the streaming state
                if has_active_stream {
                    if let Some(content) = streaming_content {
                        debug!(conv_id = %conv_id, content_len = content.len(),
                               "Restoring streaming message content from Conversation model");
                        view.start_assistant_message(cx);
                        view.append_assistant_text(&content, cx);
                    } else {
                        // Stream active but no content yet - show placeholder
                        debug!(conv_id = %conv_id, "Stream active but no content yet, starting placeholder");
                        view.start_assistant_message(cx);
                    }

                    // Restore in-progress tool trace from Conversation model
                    if let Some(trace) = streaming_trace {
                        debug!(conv_id = %conv_id, trace_items = trace.items.len(),
                               "Restoring streaming trace from Conversation model");
                        view.restore_live_trace(trace, cx);
                    }
                }
            });

            // Refresh the skills list for this conversation's effective working directory.
            // Use the conversation-level override first, then fall back to the global setting.
            let skills_dir: Option<PathBuf> = conversation_working_dir.clone().or_else(|| {
                cx.try_global::<ExecutionSettingsModel>()
                    .and_then(|s| s.workspace_dir.as_ref().map(PathBuf::from))
            });
            self.refresh_chat_input_skills(skills_dir.as_deref(), cx);
        }
    }

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
