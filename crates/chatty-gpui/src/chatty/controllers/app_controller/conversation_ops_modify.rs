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

use super::export_ops::{write_atif_export, write_jsonl_export};
use super::*;

use chatty_core::models::conversation::ConversationSync;
use chatty_core::services::hive_files::HiveAttachmentUploader;
use chatty_core::session::{
    HandoffError, HandoffServer, SendToCloud, continue_here, pick_local_model, refuse_reason,
    send_to_cloud, this_device_id,
};

use crate::chatty::views::{HandoffDialog, HandoffKind};
use crate::settings::models::hive_settings::HiveSettingsModel;

/// Whether the sidebar offers "Send to the cloud" (HX-14): only with a Hive
/// account. A conversation already in the cloud is always offered
/// "Continue on this device", signed in or not.
pub(crate) fn handoff_offered(cx: &App) -> bool {
    cx.try_global::<HiveSettingsModel>()
        .is_some_and(HiveSettingsModel::is_logged_in)
}

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
                        let local_agents = cx
                            .update(|cx| gateway_and_roster(cx, built_workspace_dir.as_deref()))
                            .map_err(|e| warn!(error = ?e, "Failed to read module gateway port"))
                            .ok()
                            .flatten();
                        // The broker starts itself on this call if it has
                        // not already (BI-2, AGE-634).
                        let lazy_broker = cx
                            .update(|cx| {
                                cx.try_global::<crate::settings::models::DiscoveredModulesModel>()
                                    .and_then(|m| m.lazy_broker.clone())
                            })
                            .ok()
                            .flatten();
                        let remote_agents = cx
                            .update(|cx| {
                                cx.try_global::<chatty_core::settings::models::extensions_store::ExtensionsModel>()
                                    .map(|m| m.a2a_agent_configs())
                                    .unwrap_or_default()
                            })
                            .unwrap_or_default();

                        // The rebuilt agent keeps raising its requests on this
                        // conversation's own session stores (AGE-272).
                        let ctx = AgentBuildContext {
                            mcp_tools,
                            shell_session,
                            theme_colors,
                            conversation_id: Some(conv_id.clone()),
                            embedded_terminals:
                                crate::chatty::views::terminal::registry::embedded_terminal_source(cx),
                            ..AgentBuildContext::from_services(AgentServices {
                                exec_settings,
                                user_secrets,
                                memory_service,
                                skill_service: Some(skill_service),
                                search_settings,
                                embedding_service,
                                lazy_broker,
                                local_agents: local_agents.unwrap_or_default(),
                                remote_agents,
                                // Not built from a spec, so no plugins (PL-U2).
                                plugin_host: Default::default(),
                            })
                        };
                        let ctx = cx
                            .update(|cx| {
                                cx.global::<ConversationsStore>()
                                    .get_session(&conv_id)
                                    .map(|session| session.build_context(ctx))
                            })
                            .map_err(|e| anyhow::anyhow!(e.to_string()))?
                            .ok_or_else(|| anyhow::anyhow!("Conversation not found"))?;

                        // Factory creates shell session on-demand if not provided
                        let built_agent = AgentClient::from_model_config_with_tools(
                            &model_config,
                            &provider_config,
                            ctx,
                        )
                        .await?;

                        // Update the conversation's agent synchronously
                        cx.update_global::<ConversationsStore, _>(|store, cx| {
                            // The settings the tools were built with are the
                            // session's too.
                            let config = desktop_session_config(cx);
                            let session = store
                                .get_session_mut(&conv_id)
                                .ok_or_else(|| anyhow::anyhow!("Conversation not found"))?;
                            session.set_config(config);
                            debug!("Updating conversation model");
                            if session.install_agent(
                                built_agent,
                                &model_config,
                                built_workspace_dir,
                            ) {
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
                                    conv.to_conversation_data()
                                        .map_err(|e| {
                                            warn!(error = ?e, "Failed to serialize conversation for save after model change");
                                        })
                                        .ok()
                                })
                            });

                        match conv_data_res {
                            Ok(Some(conv_data)) => {
                                repo.save(&conv_id, conv_data)
                                    .await
                                    .map_err(|e| anyhow::anyhow!(e))?;
                                debug!("Conversation saved to disk");
                            }
                            Ok(None) => {
                                warn!(conv_id = %conv_id, "Skipped saving conversation after model change: conversation missing or unserializable");
                            }
                            Err(e) => {
                                warn!(conv_id = %conv_id, error = ?e, "Failed to read conversation for save after model change");
                            }
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

    /// Open the confirmation for "Send to the cloud" or "Continue on this
    /// device" (HX-14). The direction is read from where the conversation's
    /// turns run now, so the dialog and the menu entry cannot disagree.
    ///
    /// Refused mid-turn: a turn can be blocked on an approval or a
    /// clarification that lives in the stores of the session that raised it.
    pub(super) fn confirm_handoff(&mut self, id: &str, cx: &mut Context<Self>) {
        let conv_id = id.to_string();
        if !cx.global::<ConversationsStore>().is_loaded(&conv_id) {
            // The hand-off needs the history, and only a loaded conversation
            // has it. Loading also opens it, which is the one the user is
            // acting on anyway; the dialog reads the direction from the row.
            self.load_conversation(&conv_id, cx);
        }
        let store = cx.global::<ConversationsStore>();
        let (sync, local_model) = store
            .get_conversation(&conv_id)
            .map(|conversation| {
                (
                    conversation.sync().clone(),
                    conversation.model_id().to_string(),
                )
            })
            .unwrap_or_else(|| {
                let sync = if store.is_hosted(&conv_id) {
                    ConversationSync::cloud(String::new(), String::new(), None)
                } else {
                    ConversationSync::LocalOnly
                };
                (sync, String::new())
            });
        let to_cloud = !sync.runs_in_cloud();
        let turn_active = store.is_streaming(&conv_id);

        if let Some(reason) = refuse_reason(turn_active, &sync, to_cloud) {
            self.chat_view.update(cx, |view, cx| {
                view.add_info_message(reason.to_string(), cx);
            });
            return;
        }
        let kind = if to_cloud {
            HandoffKind::SendToCloud
        } else {
            HandoffKind::ContinueHere
        };
        open_handoff_dialog(conv_id, kind, local_model, cx);
    }

    /// The account's Hive server, signed in as the user.
    fn handoff_server(cx: &App) -> HandoffServer {
        HandoffServer::new(
            &cx.global::<HiveSettingsModel>().hive_url,
            cx.global::<ConversationsStore>().hosted_auth(),
        )
    }

    /// Carry out a confirmed "Send to the cloud". `cloud_model` is the model
    /// the user picked when the conversation's own does not run in the cloud.
    ///
    /// Nothing local changes until every step on the server succeeded
    /// (AGE-298's rule, kept): a failure, or the app quitting part-way,
    /// leaves the conversation exactly as it was.
    pub fn send_conversation_to_cloud(
        &mut self,
        id: &str,
        cloud_model: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let conv_id = id.to_string();
        let snapshot = cx
            .global::<ConversationsStore>()
            .get_conversation(&conv_id)
            .map(|conversation| (conversation.snapshot(), conversation.model_id().to_string()));
        let Some((snapshot, local_model)) = snapshot else {
            warn!(conv_id = %conv_id, "Cannot send a conversation that is not loaded");
            self.notice(
                "This conversation is still loading. Try again in a moment.",
                cx,
            );
            return;
        };
        let server = Self::handoff_server(cx);
        let uploader = HiveAttachmentUploader::new(
            server.url(),
            cx.global::<ConversationsStore>().hosted_auth(),
        );

        cx.spawn(async move |weak, cx| {
            let result = async {
                let data = snapshot.to_data().map_err(HandoffError::Server)?;
                send_to_cloud(
                    &server,
                    SendToCloud {
                        conversation: &data,
                        sync: &snapshot.sync,
                        cloud_model: cloud_model.as_deref(),
                    },
                    &uploader,
                )
                .await
            }
            .await;

            weak.update(cx, |app, cx| match result {
                Ok(sync) => {
                    cx.update_global::<ConversationsStore, _>(|store, _cx| {
                        store.set_conversation_sync(&conv_id, sync);
                    });
                    app.persist_conversation(&conv_id, cx);
                    app.refresh_sidebar(cx);
                    app.load_conversation(&conv_id, cx);
                    app.notice(
                        "This conversation now runs in the cloud. You can close this computer \
                         and open it from any device signed in to your Hive account.",
                        cx,
                    );
                }
                Err(error @ HandoffError::HeldElsewhere { .. }) => {
                    let message = error.to_string();
                    open_handoff_dialog(conv_id.clone(), HandoffKind::TakeOver { message }, local_model.clone(), cx);
                }
                Err(error) => {
                    warn!(conv_id = %conv_id, error = ?error, "Sending the conversation to the cloud failed");
                    app.notice(&format!("Could not send this conversation to the cloud. {error}"), cx);
                }
            })
            .map_err(|e| warn!(error = ?e, "The hand-off finished but the view could not be updated"))
            .ok();
            Ok::<_, anyhow::Error>(())
        })
        .detach();
    }

    /// Carry out a confirmed "Continue on this device": pull the latest
    /// copy, take the conversation for this device, and run its next turn
    /// here on a local model. Also how a conversation another device holds
    /// is taken over. Nothing local changes unless both steps succeeded.
    pub fn continue_conversation_here(&mut self, id: &str, cx: &mut Context<Self>) {
        let conv_id = id.to_string();
        let Some(sync) = cx
            .global::<ConversationsStore>()
            .get_conversation(&conv_id)
            .map(|conversation| conversation.sync().clone())
        else {
            self.notice(
                "This conversation is still loading. Try again in a moment.",
                cx,
            );
            return;
        };
        let server = Self::handoff_server(cx);
        let cached_device = cx.global::<ConversationsStore>().this_device();

        cx.spawn(async move |weak, cx| {
            let result = async {
                let device_id = match cached_device {
                    Some(device_id) => device_id,
                    None => this_device_id(&server).await?,
                };
                let here = continue_here(&server, &sync, &device_id).await?;
                Ok::<_, HandoffError>((here, device_id))
            }
            .await;
            weak.update(cx, |app, cx| {
                let here = match result {
                    Ok((here, device_id)) => {
                        cx.update_global::<ConversationsStore, _>(|store, _cx| {
                            store.set_this_device(Some(device_id));
                        });
                        here
                    }
                    Err(error) => {
                        warn!(conv_id = %conv_id, error = ?error, "Continuing the conversation here failed");
                        app.notice(&format!("Could not continue this conversation here. {error}"), cx);
                        return;
                    }
                };
                let (missed, current_model) = cx.update_global::<ConversationsStore, _>(|store, _cx| {
                    // This device applied every event of every cloud turn it
                    // watched, so its copy is usually complete. The cloud's
                    // copy is adopted when it is longer: turns run while this
                    // device was away. `replace_from_remote` keeps the traces
                    // and per-message metadata either side has (AGE-2088).
                    let local_len = store
                        .get_conversation(&conv_id)
                        .map(|conversation| conversation.messages().len())
                        .unwrap_or(0);
                    let missed = here.remote.messages.len().saturating_sub(local_len);
                    if missed > 0
                        && let Some(conversation) = store.get_conversation_mut(&conv_id)
                        && let Err(error) = conversation
                            .replace_from_remote(here.remote.to_conversation_data(&conv_id))
                    {
                        warn!(conv_id = %conv_id, error = ?error, "Could not adopt the cloud's copy");
                    }
                    store.set_conversation_sync(&conv_id, here.sync);
                    let current = store
                        .get_conversation(&conv_id)
                        .map(|conversation| conversation.model_id().to_string())
                        .unwrap_or_default();
                    (missed, current)
                });

                app.persist_conversation(&conv_id, cx);
                app.refresh_sidebar(cx);
                app.load_conversation(&conv_id, cx);

                // The next turn runs on a local model: the cloud's when this
                // device has it, else the one the conversation had here.
                let models = cx.global::<ModelsModel>();
                let local = pick_local_model(
                    &here.remote.model_id,
                    &current_model,
                    models.default_model().map(|model| model.id.as_str()),
                    |id| models.get_model(id).is_some(),
                );
                match local {
                    Some(model) if model != current_model => app.change_conversation_model(model, cx),
                    Some(_) => {}
                    None => app.notice(
                        "Add a model in Settings → Models & Providers to continue this conversation here.",
                        cx,
                    ),
                }
                app.notice(
                    &match missed {
                        0 => "This conversation runs on this device again. The cloud copy stays in your Hive account.".to_string(),
                        n => format!(
                            "This conversation runs on this device again, with {n} message(s) from the cloud. The cloud copy stays in your Hive account."
                        ),
                    },
                    cx,
                );
            })
            .map_err(|e| warn!(error = ?e, "The hand-off finished but the view could not be updated"))
            .ok();
            Ok::<_, anyhow::Error>(())
        })
        .detach();
    }

    /// One line in the transcript.
    fn notice(&self, text: &str, cx: &mut Context<Self>) {
        let text = text.to_string();
        self.chat_view
            .update(cx, |view, cx| view.add_info_message(text, cx));
    }

    /// Delete a conversation
    pub(super) fn delete_conversation(&mut self, id: &str, cx: &mut Context<Self>) {
        let conv_id = id.to_string();
        let repo = self.conversation_repo.clone();
        let sidebar = self.sidebar_view.clone();
        let chat_view = self.chat_view.clone();

        // Drop the browser-manager registration (AGE-155) — a deleted
        // conversation's screencast, if any, has nothing left to be shown in.
        chatty_core::services::browser::registry::unregister(&conv_id);

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
        self.persist_and_export_conversation(conv_id, false, false, cx);
    }

    /// Persist a conversation to disk asynchronously, optionally also writing
    /// the ATIF / JSONL training exports from the same `ConversationData` —
    /// built once, off the UI thread, and reused for the save and each
    /// requested export instead of re-serializing per consumer (finding F3,
    /// AGE-220).
    pub(super) fn persist_and_export_conversation(
        &self,
        conv_id: &str,
        export_atif: bool,
        export_jsonl: bool,
        cx: &mut Context<Self>,
    ) {
        let conv_id = conv_id.to_string();
        let repo = self.conversation_repo.clone();

        // Cheap clone of the fields the row is built from — the actual
        // serde_json::to_string work happens in the spawned task below, off
        // the UI thread (AGE-220, finding F3).
        let snapshot = cx.update_global::<ConversationsStore, _>(|store, _cx| {
            store.get_conversation(&conv_id).map(Conversation::snapshot)
        });

        let Some(snapshot) = snapshot else {
            error!(conv_id = %conv_id, "Conversation not found for persistence");
            return;
        };

        // Update metadata immediately from typed fields so the sidebar
        // reflects title/cost right away, without waiting on the deferred
        // serialization below.
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let total_cost = snapshot.token_usage.total_estimated_cost_usd;
        cx.update_global::<ConversationsStore, _>(|store, _| {
            store.upsert_metadata(&snapshot.id, &snapshot.title, total_cost, now);
        });

        let model_config: Option<ModelConfig> = if export_atif || export_jsonl {
            cx.global::<ModelsModel>()
                .get_model(&snapshot.model_id)
                .cloned()
        } else {
            None
        };

        cx.spawn(async move |_, _cx| {
            let conv_data = match snapshot.to_data() {
                Ok(data) => data,
                Err(e) => {
                    error!(conv_id = %conv_id, error = ?e, "Failed to build conversation data for persistence (serialization failed)");
                    return Ok::<_, anyhow::Error>(());
                }
            };

            debug!(
                conv_id = %conv_id,
                traces_json_len = conv_data.system_traces.len(),
                history_json_len = conv_data.message_history.len(),
                "Persisting conversation data"
            );

            if let Err(e) = repo.save(&conv_id, conv_data.clone()).await {
                warn!(error = ?e, conv_id = %conv_id, "Failed to save conversation to disk");
            } else {
                debug!(conv_id = %conv_id, "Conversation saved to disk");
            }

            if export_atif {
                write_atif_export(&conv_id, &conv_data, model_config.as_ref()).await;
            }
            if export_jsonl {
                write_jsonl_export(&conv_id, &conv_data, model_config.as_ref()).await;
            }

            Ok(())
        })
        .detach();
    }
}

/// Open the hand-off dialog in the main window, after the current update.
fn open_handoff_dialog(conv_id: String, kind: HandoffKind, local_model: String, cx: &mut App) {
    cx.defer(move |cx| {
        // The main window's root view is gpui-component's `Root`, not
        // `ChattyApp`, so the handle is used untyped.
        let Some(window) = cx.windows().first().copied() else {
            warn!(conv_id = %conv_id, "No window to open the hand-off dialog in");
            return;
        };
        window
            .update(cx, |_, window, cx| {
                HandoffDialog::open(conv_id.clone(), kind, local_model, window, cx);
            })
            .map_err(|e| warn!(error = ?e, "Failed to open the hand-off dialog"))
            .ok();
    });
}

#[cfg(test)]
mod tests {
    use super::handoff_offered;
    use crate::settings::models::hive_settings::HiveSettingsModel;

    /// HX-14: the hand-off is offered with a Hive account and not without;
    /// there is no developer switch any more.
    #[gpui::test]
    fn send_to_the_cloud_is_offered_only_when_signed_in(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            assert!(!handoff_offered(cx), "no settings loaded yet");
            cx.set_global(HiveSettingsModel::default());
            assert!(!handoff_offered(cx), "signed out");
            cx.global_mut::<HiveSettingsModel>().token = Some("tok".into());
            assert!(handoff_offered(cx), "signed in");
        });
    }
}
