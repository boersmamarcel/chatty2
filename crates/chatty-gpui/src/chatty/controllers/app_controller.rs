use gpui::*;
use gpui_component::ActiveTheme;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;
use tracing::{debug, error, info, warn};

use crate::MemoryInitSignal;
use crate::chatty::exporters::atif_exporter::conversation_to_atif;
use crate::chatty::exporters::jsonl_exporter::{
    SftExportOptions, append_jsonl_with_dedup, conversation_to_dpo_jsonl, conversation_to_sft_jsonl,
};
use crate::chatty::factories::AgentClient;
use crate::chatty::models::token_usage::TokenUsage;
use crate::chatty::models::{
    Conversation, ConversationsStore, GlobalStreamManager, MessageFeedback, StreamManagerEvent,
    StreamStatus,
};
use crate::chatty::repositories::{ConversationData, ConversationRepository};
use crate::chatty::services::StreamChunk;
use crate::chatty::services::{generate_title, simplify_memory_query, stream_prompt};
use crate::chatty::token_budget::{
    GlobalTokenBudget, check_pressure, compute_snapshot_background, extract_user_message_text,
    gather_snapshot_inputs, summarize_oldest_half,
};
use crate::chatty::views::chat_input::{ChatInputEvent, ChatInputState};
use crate::chatty::views::chat_view::ChatViewEvent;
use crate::chatty::views::message_types::{
    ApprovalBlock, ApprovalState, SystemTrace, ThinkingState, ToolCallBlock, ToolCallState,
    TraceItem, friendly_tool_name, is_denial_result,
};
use crate::chatty::views::sidebar_view::SidebarEvent;
use crate::chatty::views::{ChatView, SidebarView};
use crate::settings::models::TokenTrackingSettings;
use crate::settings::models::execution_settings::ExecutionSettingsModel;
use crate::settings::models::models_store::{ModelConfig, ModelsModel};
use crate::settings::models::providers_store::ProviderModel;
use crate::settings::models::training_settings::TrainingSettingsModel;
use crate::settings::models::{AgentConfigEvent, AgentConfigNotifier, GlobalAgentConfigNotifier};

/// Wait for the memory service to finish initializing (with a timeout), then return it.
///
/// Returns `None` if memory is disabled in settings, if init failed, or if the
/// timeout expires. This prevents a race condition where conversations are created
/// before the `MemoryService` global has been set.
async fn await_memory_service(
    cx: &gpui::AsyncApp,
) -> Option<crate::chatty::services::MemoryService> {
    // Check if memory is enabled in settings — if not, short-circuit
    let memory_enabled = cx
        .update(|cx| {
            cx.try_global::<crate::settings::models::ExecutionSettingsModel>()
                .map(|s| s.memory_enabled)
                .unwrap_or(true)
        })
        .unwrap_or(true);

    if !memory_enabled {
        info!("await_memory_service: memory disabled by settings");
        return None;
    }

    // Grab the watch receiver (set synchronously in main.rs before the window opens)
    let receiver = cx
        .update(|cx| cx.try_global::<MemoryInitSignal>().map(|s| s.0.clone()))
        .ok()
        .flatten();

    if let Some(mut rx) = receiver
        && !*rx.borrow()
    {
        info!("await_memory_service: waiting for memory init signal");
        // Timeout is intentional: fail-open. If memory init stalls or fails,
        // we proceed without memory tools rather than blocking the user.
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            rx.wait_for(|ready| *ready),
        )
        .await;
    }

    // Now try to read the global (may still be None if init failed)
    let result = cx
        .update(|cx| {
            cx.try_global::<crate::chatty::services::MemoryService>()
                .cloned()
        })
        .ok()
        .flatten();

    if result.is_some() {
        info!("await_memory_service: MemoryService available");
    } else {
        warn!("await_memory_service: MemoryService NOT available (init failed or not set)");
    }

    result
}

/// Read the EmbeddingService from globals if available.
fn get_embedding_service(cx: &gpui::AsyncApp) -> Option<chatty_core::services::EmbeddingService> {
    cx.update(|cx| {
        cx.try_global::<chatty_core::services::EmbeddingService>()
            .cloned()
    })
    .ok()
    .flatten()
}

/// Read the SkillService from globals.
///
/// The global is set at startup (keyword-only) and replaced with an embedding-aware
/// version once the EmbeddingService is initialised.  Falls back to constructing a
/// keyword-only service if the global is absent (e.g. in tests).
fn get_skill_service(cx: &gpui::AsyncApp) -> chatty_core::services::SkillService {
    cx.update(|cx| {
        cx.try_global::<chatty_core::services::SkillService>()
            .cloned()
    })
    .ok()
    .flatten()
    .unwrap_or_else(|| {
        warn!("SkillService global not found, falling back to keyword-only service");
        chatty_core::services::SkillService::new(None)
    })
}

fn normalize_workspace_path(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn normalize_workspace_string(path: &str) -> String {
    normalize_workspace_path(Path::new(path))
        .to_string_lossy()
        .to_string()
}

async fn rebuild_conversation_agent(conv_id: &str, cx: &gpui::AsyncApp) -> anyhow::Result<()> {
    let conv_id = conv_id.to_string();

    let (model_config, provider_config) = cx
        .update(|cx| {
            let model_id = cx
                .global::<ConversationsStore>()
                .get_conversation(&conv_id)
                .map(|c| c.model_id().to_string())?;
            let models = cx.global::<ModelsModel>();
            let providers = cx.global::<ProviderModel>();
            let model_config = models.get_model(&model_id).cloned()?;
            let provider_config = providers
                .providers()
                .iter()
                .find(|p| p.provider_type == model_config.provider_type)
                .cloned()?;
            Some((model_config, provider_config))
        })
        .map_err(|e| anyhow::anyhow!(e.to_string()))?
        .ok_or_else(|| anyhow::anyhow!("Missing model/provider config for conversation rebuild"))?;

    let mcp_service = cx
        .update(|cx| cx.global::<crate::chatty::services::McpService>().clone())
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;

    let mcp_tools = mcp_service
        .get_all_tools_with_sinks()
        .await
        .map_err(|e| warn!(error = ?e, "Failed to get MCP tools"))
        .ok();
    let mcp_tools = mcp_tools.and_then(|tools| if tools.is_empty() { None } else { Some(tools) });

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
            let conv = cx.global::<ConversationsStore>().get_conversation(&conv_id);
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
            let isolation_changed = conv
                .and_then(|c| c.shell_session())
                .map(|s| s.network_isolation() != settings.network_isolation)
                .unwrap_or(false);
            let workspace_changed = conv
                .and_then(|c| c.shell_session())
                .map(|sess| {
                    sess.workspace_dir()
                        .map(|dir| normalize_workspace_path(Path::new(dir)))
                        != built_workspace_dir
                })
                .unwrap_or(false);
            let session = conv.and_then(|c| c.shell_session()).and_then(|s| {
                if !isolation_changed && !workspace_changed {
                    Some(s)
                } else {
                    info!(
                        conv_id = %conv_id,
                        isolation_changed,
                        workspace_changed,
                        "Shell session replaced due to settings change"
                    );
                    None
                }
            });
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

    let memory_service = await_memory_service(cx).await;
    let embedding_service = get_embedding_service(cx);

    let (new_agent, new_shell_session) = AgentClient::from_model_config_with_tools(
        &model_config,
        &provider_config,
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
    )
    .await?;

    cx.update_global::<ConversationsStore, _>(|store, _cx| {
        if let Some(conv) = store.get_conversation_mut(&conv_id) {
            conv.set_agent(
                new_agent,
                model_config.id.clone(),
                built_workspace_dir.clone(),
            );
            if new_shell_session.is_some() {
                conv.set_shell_session(new_shell_session);
            }
            info!(conv_id = %conv_id, "Agent successfully rebuilt with updated tool set");
        } else {
            warn!(
                conv_id = %conv_id,
                "Conversation not found during agent rebuild — skipping"
            );
        }
    })
    .map_err(|e| anyhow::anyhow!(e.to_string()))?;

    Ok(())
}

/// Global state to hold the main ChattyApp entity
#[derive(Default)]
pub struct GlobalChattyApp {
    pub entity: Option<WeakEntity<ChattyApp>>,
}

impl Global for GlobalChattyApp {}

fn push_markdown_code_block(md: &mut String, language: &str, body: &str) {
    if body.trim().is_empty() {
        return;
    }

    md.push_str(&format!("```{language}\n{body}\n```\n\n"));
}

fn push_system_trace_markdown(md: &mut String, trace_json: &serde_json::Value) {
    match serde_json::from_value::<SystemTrace>(trace_json.clone()) {
        Ok(trace) if trace.has_items() => {
            md.push_str("### Trace\n\n");

            for (index, item) in trace.items.iter().enumerate() {
                match item {
                    TraceItem::Thinking(thinking) => {
                        let status = match thinking.state {
                            ThinkingState::Processing => "running",
                            ThinkingState::Completed => "completed",
                        };
                        md.push_str(&format!("{}. **Thinking** ({status})\n", index + 1));

                        if !thinking.summary.trim().is_empty() {
                            md.push_str(&format!("   - Summary: {}\n", thinking.summary.trim()));
                        }

                        if !thinking.content.trim().is_empty() {
                            md.push_str("   - Details:\n\n");
                            push_markdown_code_block(md, "text", thinking.content.trim());
                        } else {
                            md.push('\n');
                        }
                    }
                    TraceItem::ToolCall(tool_call) => {
                        let status = match &tool_call.state {
                            ToolCallState::Running => "running".to_string(),
                            ToolCallState::Success => "success".to_string(),
                            ToolCallState::Error(err) => format!("error: {err}"),
                        };

                        md.push_str(&format!(
                            "{}. **Tool:** `{}` ({status})\n",
                            index + 1,
                            tool_call.display_name
                        ));

                        if !tool_call.input.trim().is_empty() {
                            md.push_str("   - Input:\n\n");
                            push_markdown_code_block(md, "text", tool_call.input.trim());
                        }

                        if let Some(output) = tool_call.output.as_deref()
                            && !output.trim().is_empty()
                        {
                            md.push_str("   - Output:\n\n");
                            push_markdown_code_block(md, "text", output.trim());
                        } else if let Some(output_preview) = tool_call.output_preview.as_deref()
                            && !output_preview.trim().is_empty()
                        {
                            md.push_str("   - Output Preview:\n\n");
                            push_markdown_code_block(md, "text", output_preview.trim());
                        } else {
                            md.push('\n');
                        }
                    }
                    TraceItem::ApprovalPrompt(approval) => {
                        let status = match approval.state {
                            ApprovalState::Pending => "pending",
                            ApprovalState::Approved => "approved",
                            ApprovalState::Denied => "denied",
                        };

                        md.push_str(&format!(
                            "{}. **Approval** ({status})\n   - Command: `{}`\n\n",
                            index + 1,
                            approval.command
                        ));
                    }
                }
            }
        }
        Ok(_) => {}
        Err(error) => {
            warn!(error = ?error, "Failed to deserialize trace for markdown export");
            md.push_str("### Trace (raw)\n\n");
            match serde_json::to_string_pretty(trace_json) {
                Ok(raw_json) => push_markdown_code_block(md, "json", &raw_json),
                Err(error) => {
                    warn!(error = ?error, "Failed to pretty-print raw trace JSON for export");
                }
            }
        }
    }
}

pub struct ChattyApp {
    pub chat_view: Entity<ChatView>,
    pub sidebar_view: Entity<SidebarView>,
    conversation_repo: Arc<dyn ConversationRepository>,
    is_ready: bool,
    /// Held while a conversation is being created; prevents concurrent creations.
    /// Automatically dropped (and thus "cleared") when the task completes.
    active_create_task: Option<Task<anyhow::Result<String>>>,
    /// Keeps the AgentConfigNotifier entity alive for the app's lifetime so that
    /// GlobalAgentConfigNotifier's WeakEntity remains upgradeable.
    _mcp_notifier: Entity<AgentConfigNotifier>,
}

impl ChattyApp {
    pub fn new(
        window: &mut Window,
        cx: &mut Context<Self>,
        conversation_repo: Arc<dyn ConversationRepository>,
    ) -> Self {
        // Initialize global conversations model if not already done
        if !cx.has_global::<ConversationsStore>() {
            cx.set_global(ConversationsStore::new());
        }

        // Create views
        let chat_view = cx.new(|cx| ChatView::new(window, cx));
        let sidebar_view = cx.new(|_cx| SidebarView::new());

        // Create the agent config notifier and keep the strong entity alive in ChattyApp
        // so GlobalAgentConfigNotifier's WeakEntity remains upgradeable for the app's lifetime.
        let mcp_notifier = cx.new(|_cx| AgentConfigNotifier::new());
        cx.set_global(GlobalAgentConfigNotifier {
            entity: Some(mcp_notifier.downgrade()),
        });

        let app = Self {
            chat_view,
            sidebar_view,
            conversation_repo,
            is_ready: false,
            active_create_task: None,
            _mcp_notifier: mcp_notifier,
        };

        // Store entity in global state for later access
        let app_weak = cx.entity().downgrade();
        if !cx.has_global::<GlobalChattyApp>() {
            cx.set_global(GlobalChattyApp {
                entity: Some(app_weak),
            });
        } else {
            cx.update_global::<GlobalChattyApp, _>(|global, _| {
                global.entity = Some(app_weak);
            });
        }

        // Set up callbacks
        app.setup_callbacks(cx);

        // Initialize chat input with available models
        app.initialize_models(cx);

        // is_ready is set by load_conversations_after_models_ready() once disk load completes.
        // Do NOT create an initial conversation here — ConversationsStore is always empty at
        // this point because disk loading hasn't happened yet. Creating one here causes a race
        // condition where a blank conversation appears instead of the user's history.

        app
    }

    /// Load conversations after models and providers are ready
    /// This should be called from main.rs after both models and providers have been loaded
    pub fn load_conversations_after_models_ready(&self, cx: &mut Context<Self>) {
        info!("Starting conversation load");
        self.load_conversations(cx);
    }

    /// Set up all event subscriptions between components
    ///
    /// All entity-to-entity communication uses EventEmitter/cx.subscribe():
    /// 1. SidebarView emits SidebarEvent → ChattyApp handles
    /// 2. ChatInputState emits ChatInputEvent → ChattyApp handles
    /// 3. AgentConfigNotifier emits AgentConfigEvent → ChattyApp handles
    /// 4. StreamManager emits StreamManagerEvent → ChattyApp handles
    fn setup_callbacks(&self, cx: &mut Context<Self>) {
        // SUBSCRIPTION 1: SidebarView events
        cx.subscribe(
            &self.sidebar_view,
            |app, _sidebar, event: &SidebarEvent, cx| match event {
                SidebarEvent::NewChat => {
                    app.start_new_conversation(cx);
                }
                SidebarEvent::OpenSettings => {
                    cx.defer(|cx| {
                        use crate::settings::controllers::SettingsView;
                        SettingsView::open_or_focus_settings_window(cx);
                    });
                }
                SidebarEvent::SelectConversation(conv_id) => {
                    app.load_conversation(conv_id, cx);
                }
                SidebarEvent::DeleteConversation(conv_id) => {
                    app.delete_conversation(conv_id, cx);
                }
                SidebarEvent::ExportConversation(conv_id) => {
                    app.export_conversation_markdown(conv_id, cx);
                }
                SidebarEvent::ToggleCollapsed(collapsed) => {
                    // Optional: Could save collapsed state to settings here
                    debug!(collapsed = collapsed, "Sidebar toggled");
                }
                SidebarEvent::LoadMore => {
                    let sidebar = app.sidebar_view.clone();
                    sidebar.update(cx, |sidebar, cx| {
                        let store = cx.global::<ConversationsStore>();
                        let total = store.count();
                        let convs = store.list_recent_metadata(sidebar.visible_limit());
                        debug!(
                            conv_count = convs.len(),
                            total = total,
                            limit = sidebar.visible_limit(),
                            "Load More: Reloading conversations with new limit"
                        );
                        sidebar.set_conversations(convs, cx);
                        sidebar.set_total_count(total);
                    });
                }
            },
        )
        .detach();

        // SUBSCRIPTION 2: ChatInputState events
        let chat_input_state = self.chat_view.read(cx).chat_input_state().clone();
        cx.subscribe(
            &chat_input_state,
            |app, _input, event: &ChatInputEvent, cx| match event {
                ChatInputEvent::Send {
                    message,
                    attachments,
                } => {
                    debug!(message = %message, attachment_count = attachments.len(), "ChatInputEvent::Send received");
                    // Intercept arg-based slash commands before sending to LLM.
                    if app.try_handle_arg_slash_command(message.trim(), cx) {
                        return;
                    }
                    app.send_message(message.clone(), attachments.clone(), cx);
                }
                ChatInputEvent::ModelChanged(model_id) => {
                    debug!(model_id = %model_id, "ChatInputEvent::ModelChanged received");
                    // Defer capability update to avoid re-entering ChatInputState
                    let chat_view = app.chat_view.clone();
                    let mid = model_id.clone();
                    cx.defer(move |cx| {
                        let capabilities = cx
                            .global::<ModelsModel>()
                            .get_model(&mid)
                            .map(|m| (m.supports_images, m.supports_pdf))
                            .unwrap_or((false, false));

                        chat_view.update(cx, |view, cx| {
                            view.chat_input_state().update(cx, |state, _cx| {
                                state.set_capabilities(capabilities.0, capabilities.1);
                            });
                        });
                    });
                    app.change_conversation_model(model_id.clone(), cx);
                }
                ChatInputEvent::Stop => {
                    debug!("ChatInputEvent::Stop received");
                    app.stop_stream(cx);
                }
                ChatInputEvent::SlashCommandSelected(command) => {
                    debug!(command = %command, "ChatInputEvent::SlashCommandSelected received");
                    app.handle_slash_command(command.clone(), cx);
                }
                ChatInputEvent::WorkingDirChanged(dir) => {
                    debug!(dir = ?dir, "ChatInputEvent::WorkingDirChanged received");
                    app.change_conversation_working_dir(dir.clone(), cx);
                }
            },
        )
        .detach();

        // SUBSCRIPTION 3: McpNotifier events — rebuild agent when MCP servers change
        if let Some(weak_notifier) = cx
            .try_global::<GlobalAgentConfigNotifier>()
            .and_then(|g| g.entity.clone())
            && let Some(notifier) = weak_notifier.upgrade()
        {
            cx.subscribe(
                &notifier,
                |this, _notifier, event: &AgentConfigEvent, cx| {
                    if matches!(event, AgentConfigEvent::RebuildRequired) {
                        this.rebuild_active_agent(cx);
                    }
                },
            )
            .detach();
        }

        // SUBSCRIPTION 4: StreamManager events — decoupled UI updates
        if let Some(manager) = cx
            .try_global::<GlobalStreamManager>()
            .and_then(|g| g.entity.clone())
        {
            cx.subscribe(&manager, |app, _mgr, event: &StreamManagerEvent, cx| {
                app.handle_stream_manager_event(event, cx);
            })
            .detach();
        }

        // SUBSCRIPTION 5: ChatView events — feedback persistence
        cx.subscribe(
            &self.chat_view,
            |app, _chat_view, event: &ChatViewEvent, cx| match event {
                ChatViewEvent::FeedbackChanged {
                    history_index,
                    feedback,
                } => {
                    app.handle_feedback_changed(*history_index, feedback.clone(), cx);
                }
                ChatViewEvent::RegenerateMessage { history_index } => {
                    app.handle_regeneration(*history_index, cx);
                }
            },
        )
        .detach();
    }

    /// Initialize chat input with available models
    fn initialize_models(&self, cx: &mut Context<Self>) {
        let chat_view = self.chat_view.clone();

        // Get models from global store
        if let Some(models_model) = cx.try_global::<ModelsModel>() {
            let models_list: Vec<(String, String)> = models_model
                .models()
                .iter()
                .map(|m| (m.id.clone(), m.name.clone()))
                .collect();

            let default_model_id = models_list.first().map(|(id, _)| id.clone());

            // Get capabilities of the default model
            let default_capabilities = models_model
                .models()
                .first()
                .map(|m| (m.supports_images, m.supports_pdf))
                .unwrap_or((false, false));

            // Set available models on chat input
            chat_view.update(cx, |view, cx| {
                view.chat_input_state().update(cx, |state, _cx| {
                    state.set_available_models(models_list, default_model_id);
                    state.set_capabilities(default_capabilities.0, default_capabilities.1);
                });
            });
        }
    }

    /// Restore a single conversation from persisted data
    ///
    /// Looks up the model and provider configs, then calls Conversation::from_data()
    #[allow(clippy::too_many_arguments)]
    async fn restore_conversation_from_data(
        data: ConversationData,
        models: &ModelsModel,
        providers: &ProviderModel,
        mcp_service: &crate::chatty::services::McpService,
        exec_settings: &crate::settings::models::ExecutionSettingsModel,
        pending_approvals: crate::chatty::models::execution_approval_store::PendingApprovals,
        pending_write_approvals: crate::chatty::models::write_approval_store::PendingWriteApprovals,
        user_secrets: Vec<(String, String)>,
        theme_colors: Option<[String; 5]>,
        memory_service: Option<crate::chatty::services::MemoryService>,
        search_settings: Option<crate::settings::models::SearchSettingsModel>,
        embedding_service: Option<chatty_core::services::EmbeddingService>,
    ) -> anyhow::Result<Conversation> {
        let mut effective_exec_settings = exec_settings.clone();
        if let Some(working_dir) = data.working_dir.as_ref() {
            effective_exec_settings.workspace_dir = Some(normalize_workspace_string(working_dir));
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
        let mcp_tools = mcp_service
            .get_all_tools_with_sinks()
            .await
            .ok()
            .and_then(|tools| if tools.is_empty() { None } else { Some(tools) });

        // Restore conversation using factory method (bash tool will be created in agent_factory if enabled)
        Conversation::from_data(
            data,
            model_config,
            provider_config,
            mcp_tools,
            Some(effective_exec_settings),
            Some(pending_approvals),
            Some(pending_write_approvals),
            user_secrets,
            theme_colors,
            memory_service,
            search_settings,
            embedding_service,
        )
        .await
    }

    /// Load conversation metadata at startup (fast — no message deserialization).
    ///
    /// Only loads lightweight id/title/cost metadata for the sidebar. Full conversation
    /// data is loaded lazily when the user selects a conversation.
    fn load_conversations(&self, cx: &mut Context<Self>) {
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
                    .unwrap()
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
                    let mcp_tools = mcp_service
                        .get_all_tools_with_sinks()
                        .await
                        .ok()
                        .and_then(|tools| if tools.is_empty() { None } else { Some(tools) });

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

                    let mut conversation = Conversation::new(
                        conv_id.clone(),
                        title.clone(),
                        &model_config,
                        &provider_config,
                        mcp_tools,
                        exec_settings,
                        pending_approvals,
                        pending_write_approvals,
                        user_secrets,
                        theme_colors,
                        memory_service,
                        search_settings,
                        embedding_service,
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
                        .unwrap()
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
    fn load_conversation(&mut self, id: &str, cx: &mut Context<Self>) {
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
                            data, &models, &providers, &mcp_service, &exec_settings,
                            pending_approvals, pending_write_approvals, user_secrets,
                            theme_colors, memory_service, search_settings, embedding_service,
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
                .and_then(|g| g.entity.clone())
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
                // Note: We clone traces here, but they're just Vec<Option<serde_json::Value>>
                // The actual trace deserialization happens lazily when user expands them
                let conversation_data = cx.global::<ConversationsStore>()
                    .get_conversation(&conv_id)
                    .map(|conv| {
                        (
                            conv.history().to_vec(),
                            conv.system_traces().to_vec(),
                            conv.attachment_paths().to_vec(),
                            conv.message_feedback().to_vec(),
                        )
                    });

                if let Some((history, traces, attachment_paths, feedback)) = conversation_data {
                    view.load_history(&history, &traces, &attachment_paths, &feedback, cx);
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
                    state.set_working_dir_silent(conversation_working_dir);
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
        if let Some(manager) = cx
            .try_global::<GlobalStreamManager>()
            .and_then(|g| g.entity.clone())
        {
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
    fn rebuild_active_agent(&mut self, cx: &mut Context<Self>) {
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

    fn change_conversation_model(&mut self, model_id: String, cx: &mut Context<Self>) {
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

                        // Get MCP tools from active servers (outside of cx.update)
                        let mcp_tools = mcp_service
                            .get_all_tools_with_sinks()
                            .await
                            .map_err(|e| warn!(error = ?e, "Failed to get MCP tools"))
                            .ok();

                        let mcp_tools = mcp_tools
                            .and_then(|tools| if tools.is_empty() { None } else { Some(tools) });

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

                        // Factory creates shell session on-demand if not provided
                        let (new_agent, new_shell_session) =
                            AgentClient::from_model_config_with_tools(
                                &model_config,
                                &provider_config,
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
                                        .unwrap()
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
                                            .unwrap()
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
    fn change_conversation_working_dir(
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
    }

    /// Delete a conversation
    fn delete_conversation(&mut self, id: &str, cx: &mut Context<Self>) {
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

    /// Export a conversation as Markdown with an OS file-save dialog.
    ///
    /// Builds markdown from the conversation history in `ConversationsStore`,
    /// prompts the user for a save location, and writes the file asynchronously.
    fn export_conversation_markdown(&self, id: &str, cx: &mut Context<Self>) {
        let conv_id = id.to_string();

        // Build markdown from ConversationsStore (works for any conversation, not just active)
        let store = cx.global::<ConversationsStore>();
        let Some(conv) = store.get_conversation(&conv_id) else {
            warn!(conv_id = %conv_id, "Cannot export: conversation not found or has no messages");
            return;
        };

        let title = conv.title().to_string();
        let mut markdown = format!("# {title}\n\n");
        for (index, msg) in conv.history().iter().enumerate() {
            let trace_json = conv
                .system_traces()
                .get(index)
                .and_then(|trace| trace.as_ref());

            match msg {
                rig::completion::Message::User { content, .. } => {
                    let text = content
                        .iter()
                        .filter_map(|c| match c {
                            rig::completion::message::UserContent::Text(t) => Some(t.text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n\n");
                    if !text.is_empty() {
                        markdown.push_str(&format!("---\n\n**User**\n\n{text}\n\n"));
                    }
                }
                rig::completion::Message::Assistant { content, .. } => {
                    let text = content
                        .iter()
                        .filter_map(|c| match c {
                            rig::completion::message::AssistantContent::Text(t) => {
                                Some(t.text.as_str())
                            }
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n\n");
                    if !text.is_empty() || trace_json.is_some() {
                        markdown.push_str("---\n\n**Assistant**\n\n");

                        if !text.is_empty() {
                            markdown.push_str(&text);
                            markdown.push_str("\n\n");
                        }

                        if let Some(trace_json) = trace_json {
                            push_system_trace_markdown(&mut markdown, trace_json);
                        }
                    }
                }
            }
        }

        if markdown.is_empty() {
            warn!(conv_id = %conv_id, "Cannot export: conversation not found or has no messages");
            return;
        }

        let suggested = format!(
            "{}.md",
            title.replace(['/', '\\', ':', '*', '?', '"', '<', '>', '|'], "_")
        );
        let home = dirs::home_dir()
            .unwrap_or_else(|| dirs::document_dir().unwrap_or_else(|| PathBuf::from(".")));

        cx.spawn(async move |_weak, cx| {
            let receiver = cx
                .update(|cx| cx.prompt_for_new_path(&home, Some(&suggested)))
                .map_err(|e| warn!(error = ?e, "Failed to open save dialog"))
                .ok()?;
            match receiver.await {
                Ok(Ok(Some(path))) => {
                    if let Err(e) = tokio::fs::write(&path, markdown.as_bytes()).await {
                        warn!(error = ?e, path = ?path, "Failed to write markdown export");
                    }
                }
                Ok(Ok(None)) => {} // user cancelled
                Ok(Err(e)) => warn!(error = ?e, "Save dialog returned error"),
                Err(e) => warn!(error = ?e, "Failed to receive save dialog result"),
            }
            Some(())
        })
        .detach();
    }

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
    fn send_message(&mut self, message: String, attachments: Vec<PathBuf>, cx: &mut Context<Self>) {
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
        let stream_manager = cx
            .try_global::<GlobalStreamManager>()
            .and_then(|g| g.entity.clone());

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
                let (agent, history, _model_id, provider_supports_pdf, provider_supports_images, conv_attachment_paths) = cx
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
                                conv.history().to_vec(),
                                model_id,
                                supports_pdf,
                                supports_images,
                                conv.attachment_paths().to_vec(),
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
                    &history,
                    &conv_attachment_paths,
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
                    conv_id,
                    agent,
                    history,
                    contents,
                    true, // add user message to conversation model
                    attachments,
                    chat_view,
                    stream_manager,
                    cancel_flag_for_loop,
                    cx,
                )
                .await
            });

        // Register stream with StreamManager (owns task + cancel flag)
        if let Some(manager) = cx
            .try_global::<GlobalStreamManager>()
            .and_then(|g| g.entity.clone())
        {
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
    fn handle_stream_manager_event(&mut self, event: &StreamManagerEvent, cx: &mut Context<Self>) {
        let chat_view = self.chat_view.clone();

        match event {
            StreamManagerEvent::StreamStarted { conversation_id } => {
                debug!(conv_id = %conversation_id, "StreamManager: stream started");
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
                        };
                        let trace = conv.ensure_streaming_trace();
                        let index = trace.items.len();
                        trace.add_tool_call(tool_call);
                        trace.set_active_tool(index);
                    }
                });

                chat_view.update(cx, |view, cx| {
                    if view.conversation_id() == Some(conversation_id) {
                        view.handle_tool_call_started(id, name, cx);
                    }
                });
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
                            tc.input = args;
                        }) {
                            warn!(tool_id = %id, "ToolCallInput: tool call not found in model trace");
                        }
                    }
                });

                chat_view.update(cx, |view, cx| {
                    if view.conversation_id() == Some(conversation_id) {
                        view.handle_tool_call_input(id, arguments, cx);
                    }
                });
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
                            tc.output = Some(res);
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

                chat_view.update(cx, |view, cx| {
                    if view.conversation_id() == Some(conversation_id) {
                        view.handle_tool_call_result(id, result, cx);
                    }
                });
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

                chat_view.update(cx, |view, cx| {
                    if view.conversation_id() == Some(conversation_id) {
                        view.handle_tool_call_error(id, error, cx);
                    }
                });
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
        if let Some(manager) = cx
            .try_global::<GlobalStreamManager>()
            .and_then(|g| g.entity.clone())
        {
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

    // -----------------------------------------------------------------------
    // Slash-command handlers
    // -----------------------------------------------------------------------

    /// Dispatch a slash command that was selected from the picker.
    fn handle_slash_command(&mut self, command: String, cx: &mut Context<Self>) {
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
                .map(|conv| (conv.agent().clone(), conv.history().to_vec()))
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
    fn try_handle_arg_slash_command(&mut self, text: &str, cx: &mut Context<Self>) -> bool {
        if let Some(prompt) = text.strip_prefix("/agent ") {
            let prompt = prompt.trim().to_string();
            if prompt.is_empty() {
                self.chat_view.update(cx, |view, cx| {
                    view.add_info_message(
                        "Usage: `/agent <prompt>` — provide a prompt for the sub-agent."
                            .to_string(),
                        cx,
                    );
                });
            } else {
                self.launch_agent(prompt, cx);
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

    /// `/agent <prompt>` — launch chatty-tui in headless mode with the given prompt.
    fn launch_agent(&mut self, prompt: String, cx: &mut Context<Self>) {
        info!(prompt = %prompt, "Slash command: launch sub-agent");

        // Capture the conversation where the sub-agent is launched so the result can be
        // routed back to the correct conversation even if the user navigates away.
        let launch_conv_id = cx
            .try_global::<ConversationsStore>()
            .and_then(|store| store.active_id().cloned());

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
            view.start_sub_agent_progress(&prompt_for_display, cx);
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
                        for line in reader.lines().flatten() {
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
            // agent can reference it on subsequent turns.  We use a User message so
            // that the LLM sees the content as context it can act on.
            if let (Some(conv_id), Some(txt)) = (&launch_conv_id, &result_text) {
                let history_entry = rig::completion::Message::User {
                    content: rig::OneOrMany::one(rig::message::UserContent::text(format!(
                        "[Sub-agent result for: {prompt_label}]\n\n{txt}",
                    ))),
                };
                cx.update(|cx| {
                    cx.update_global::<ConversationsStore, _>(|store, _cx| {
                        if let Some(conv) = store.get_conversation_mut(conv_id) {
                            conv.add_user_message_with_attachments(history_entry, vec![]);
                        }
                    });
                })
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
                                    .and_then(|g| g.entity.clone())
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
                    let traces_len = conv.system_traces().len();
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
                                    .map(|conv| (conv.agent().clone(), conv.history().to_vec()))
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
                            .map(|conv| (conv.agent().clone(), conv.history().to_vec()))
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
                                    .unwrap()
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
        // and save to conversation history
        let assistant_history_index = cx.update_global::<ConversationsStore, _>(|store, _cx| {
            if let Some(conv) = store.get_conversation_mut(&conv_id) {
                let partial_text = conv.streaming_message().cloned().unwrap_or_default();
                conv.finalize_response(partial_text, Vec::new(), trace_json);
                conv.set_streaming_message(None);
                let idx = conv.message_count().saturating_sub(1);
                debug!(conv_id = %conv_id, "Partial response saved to conversation after stop");
                Some(idx)
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

    /// Refresh the sidebar with the latest conversation list from the metadata store
    fn refresh_sidebar(&self, cx: &mut Context<Self>) {
        self.sidebar_view.update(cx, |sidebar, cx| {
            let store = cx.global::<ConversationsStore>();
            let total = store.count();
            let convs = store.list_recent_metadata(sidebar.visible_limit());
            sidebar.set_conversations(convs, cx);
            sidebar.set_total_count(total);
        });
    }

    /// Handle feedback change: update ConversationsStore and persist
    fn handle_feedback_changed(
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
    fn handle_regeneration(&mut self, history_index: usize, cx: &mut Context<Self>) {
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

        let stream_manager = cx
            .try_global::<GlobalStreamManager>()
            .and_then(|g| g.entity.clone());

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
            let (agent, history) = cx
                .update_global::<ConversationsStore, _>(|store, _cx| {
                    if let Some(conv) = store.get_conversation(&conv_id) {
                        if let Ok(mut artifacts) = conv.pending_artifacts().lock() {
                            artifacts.clear();
                        }
                        Ok((conv.agent().clone(), conv.history().to_vec()))
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
                conv_id,
                agent,
                history_context,
                user_contents,
                false, // user message already in model
                vec![],
                chat_view,
                stream_manager,
                cancel_flag_for_loop,
                cx,
            )
            .await
        });

        // Register stream with StreamManager
        if let Some(manager) = cx
            .try_global::<GlobalStreamManager>()
            .and_then(|g| g.entity.clone())
        {
            manager.update(cx, |mgr, cx| {
                mgr.register_stream(conv_id_for_task, task, cancel_flag, pending_artifacts, cx);
            });
        } else {
            error!("StreamManager not available for regeneration stream");
        }
    }

    /// Persist a conversation to disk asynchronously.
    /// Also updates the metadata store so the sidebar reflects the latest title and cost.
    fn persist_conversation(&self, conv_id: &str, cx: &mut Context<Self>) {
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

    /// Get the chat input state entity
    #[allow(dead_code)]
    pub fn chat_input_state(&self, cx: &App) -> Entity<ChatInputState> {
        self.chat_view.read(cx).chat_input_state().clone()
    }

    /// Export a conversation as ATIF JSON to the exports directory.
    ///
    /// Builds ConversationData from the store, looks up the ModelConfig for
    /// provider metadata, converts to ATIF, and writes the file asynchronously.
    fn export_conversation_atif(&self, conv_id: &str, cx: &mut Context<Self>) {
        let conv_id = conv_id.to_string();

        // Build ConversationData and get the model config (same data as persist_conversation)
        let export_data = cx.update_global::<ConversationsStore, _>(|store, _cx| {
            store
                .get_conversation(&conv_id)
                .and_then(build_conversation_data)
        });

        let Some(conv_data) = export_data else {
            warn!(conv_id = %conv_id, "Cannot export ATIF: conversation not found");
            return;
        };

        // Look up ModelConfig for provider metadata
        let model_config: Option<ModelConfig> = cx
            .global::<ModelsModel>()
            .get_model(&conv_data.model_id)
            .cloned();

        cx.spawn(async move |_, _cx| {
            // Convert to ATIF
            let atif_json = match conversation_to_atif(&conv_data, model_config.as_ref()) {
                Ok(json) => json,
                Err(e) => {
                    warn!(error = ?e, conv_id = %conv_id, "Failed to convert conversation to ATIF");
                    return Ok::<_, anyhow::Error>(());
                }
            };

            // Determine exports directory
            let exports_dir = match dirs::config_dir() {
                Some(config) => config.join("chatty").join("exports"),
                None => {
                    warn!("Cannot determine config directory for ATIF export");
                    return Ok(());
                }
            };

            // Create exports directory if needed
            if let Err(e) = tokio::fs::create_dir_all(&exports_dir).await {
                warn!(error = ?e, "Failed to create ATIF exports directory");
                return Ok(());
            }

            // Write atomically using temp file + rename
            let file_path = exports_dir.join(format!("{}.atif.json", conv_id));
            let temp_path = file_path.with_extension(format!("json.{}.tmp", std::process::id()));

            let json_str = match serde_json::to_string_pretty(&atif_json) {
                Ok(s) => s,
                Err(e) => {
                    warn!(error = ?e, conv_id = %conv_id, "Failed to serialize ATIF JSON");
                    return Ok(());
                }
            };

            if let Err(e) = tokio::fs::write(&temp_path, &json_str).await {
                warn!(error = ?e, conv_id = %conv_id, "Failed to write ATIF temp file");
                return Ok(());
            }

            if let Err(e) = tokio::fs::rename(&temp_path, &file_path).await {
                warn!(error = ?e, conv_id = %conv_id, "Failed to rename ATIF temp file");
                return Ok(());
            }

            debug!(
                conv_id = %conv_id,
                path = %file_path.display(),
                "ATIF export saved"
            );

            Ok(())
        })
        .detach();
    }

    /// Export a conversation as JSONL (SFT + DPO) to the exports directory.
    ///
    /// Builds ConversationData from the store, converts to SFT and DPO JSONL lines,
    /// and appends to sft.jsonl and dpo.jsonl with deduplication by _conversation_id.
    fn export_conversation_jsonl(&self, conv_id: &str, cx: &mut Context<Self>) {
        let conv_id = conv_id.to_string();

        // Build ConversationData (same pattern as export_conversation_atif)
        let export_data = cx.update_global::<ConversationsStore, _>(|store, _cx| {
            store
                .get_conversation(&conv_id)
                .and_then(build_conversation_data)
        });

        let Some(conv_data) = export_data else {
            warn!(conv_id = %conv_id, "Cannot export JSONL: conversation not found");
            return;
        };

        // Look up ModelConfig for system prompt
        let model_config: Option<ModelConfig> = cx
            .global::<ModelsModel>()
            .get_model(&conv_data.model_id)
            .cloned();

        cx.spawn(async move |_, _cx| {
            // Convert to SFT
            let sft_options = SftExportOptions::default();
            let sft_line =
                match conversation_to_sft_jsonl(&conv_data, model_config.as_ref(), &sft_options) {
                    Ok(line) => line,
                    Err(e) => {
                        warn!(error = ?e, conv_id = %conv_id, "Failed to convert conversation to SFT JSONL");
                        None
                    }
                };

            // Convert to DPO
            let dpo_lines = match conversation_to_dpo_jsonl(&conv_data, model_config.as_ref()) {
                Ok(lines) => lines,
                Err(e) => {
                    warn!(error = ?e, conv_id = %conv_id, "Failed to convert conversation to DPO JSONL");
                    Vec::new()
                }
            };

            // Determine exports directory
            let exports_dir = match dirs::config_dir() {
                Some(config) => config.join("chatty").join("exports"),
                None => {
                    warn!("Cannot determine config directory for JSONL export");
                    return Ok::<_, anyhow::Error>(());
                }
            };

            if let Err(e) = tokio::fs::create_dir_all(&exports_dir).await {
                warn!(error = ?e, "Failed to create JSONL exports directory");
                return Ok(());
            }

            // Append SFT line with dedup
            let has_sft = sft_line.is_some();
            if let Some(sft_val) = sft_line
                && let Err(e) = append_jsonl_with_dedup(
                    &exports_dir.join("sft.jsonl"),
                    &[sft_val],
                    &conv_id,
                )
                .await
            {
                warn!(error = ?e, conv_id = %conv_id, "Failed to write SFT JSONL");
            }

            // Append DPO lines with dedup
            let dpo_count = dpo_lines.len();
            if !dpo_lines.is_empty()
                && let Err(e) = append_jsonl_with_dedup(
                    &exports_dir.join("dpo.jsonl"),
                    &dpo_lines,
                    &conv_id,
                )
                .await
            {
                warn!(error = ?e, conv_id = %conv_id, "Failed to write DPO JSONL");
            }

            debug!(
                conv_id = %conv_id,
                has_sft = has_sft,
                dpo_count = dpo_count,
                "JSONL export saved"
            );

            Ok(())
        })
        .detach();
    }
}

/// Serialize a `Conversation` into a `ConversationData` snapshot suitable for persistence
/// or export. Returns `None` if history or traces cannot be serialized.
/// Extract the current theme's chart colors as hex strings.
///
/// These are captured at agent-creation time so that charts saved to disk by the
/// `create_chart` tool match the inline chart appearance in the app.
fn extract_theme_chart_colors(cx: &gpui::App) -> [String; 5] {
    [
        cx.theme().chart_1,
        cx.theme().chart_2,
        cx.theme().chart_3,
        cx.theme().chart_4,
        cx.theme().chart_5,
    ]
    .map(|c| {
        let r = c.to_rgb();
        format!(
            "#{:02x}{:02x}{:02x}",
            (r.r * 255.0) as u8,
            (r.g * 255.0) as u8,
            (r.b * 255.0) as u8
        )
    })
}

///
/// Sets `updated_at` to the current time; all other timestamps are taken from the
/// conversation itself.
fn build_conversation_data(conv: &Conversation) -> Option<ConversationData> {
    let history = match conv.serialize_history() {
        Ok(h) => h,
        Err(e) => {
            error!(conv_id = %conv.id(), error = ?e, "Failed to serialize history in build_conversation_data");
            return None;
        }
    };
    let traces = match conv.serialize_traces() {
        Ok(t) => t,
        Err(e) => {
            error!(conv_id = %conv.id(), error = ?e, "Failed to serialize traces in build_conversation_data");
            return None;
        }
    };
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

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
            .unwrap()
            .as_secs() as i64,
        updated_at: now,
        working_dir: conv.working_dir().map(|p| p.to_string_lossy().to_string()),
    })
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
#[allow(clippy::too_many_arguments)]
async fn run_llm_stream(
    conv_id: String,
    agent: AgentClient,
    history: Vec<rig::completion::Message>,
    user_contents: Vec<rig::message::UserContent>,
    add_user_message_to_model: bool,
    attachment_paths: Vec<PathBuf>,
    chat_view: Entity<ChatView>,
    stream_manager: Option<Entity<crate::chatty::models::StreamManager>>,
    cancel_flag: Arc<AtomicBool>,
    cx: &mut AsyncApp,
) -> anyhow::Result<()> {
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
    let workspace_dir = cx
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

    // 2c. Auto-retrieve relevant memories and inject as context
    let user_contents = {
        let memory_service = await_memory_service(cx).await;
        let embedding_service = get_embedding_service(cx);
        let skill_service = get_skill_service(cx);

        if let Some(mem_svc) = memory_service {
            let raw_text = extract_user_message_text(&user_contents);
            let query_text = simplify_memory_query(&raw_text);
            info!(
                conv_id = %conv_id,
                raw_len = raw_text.len(),
                query = %query_text,
                "Memory auto-retrieval: searching"
            );

            let workspace_skills_dir = workspace_dir
                .as_deref()
                .map(|d| std::path::Path::new(d).join(".claude").join("skills"));
            if let Some(context_block) = chatty_core::services::load_auto_context_block(
                chatty_core::services::AutoContextRequest {
                    memory_service: &mem_svc,
                    embedding_service: embedding_service.as_ref(),
                    skill_service: &skill_service,
                    query_text: &query_text,
                    fallback_query_text: Some(&raw_text),
                    workspace_skills_dir: workspace_skills_dir.as_deref(),
                },
            )
            .await
            {
                let mut augmented = vec![rig::message::UserContent::Text(
                    rig::completion::message::Text {
                        text: context_block,
                    },
                )];
                augmented.extend(user_contents);
                debug!(conv_id = %conv_id, "Injected memory context into user message");
                augmented
            } else {
                user_contents
            }
        } else {
            user_contents
        }
    };

    // 3. Call stream_prompt
    debug!(conv_id = %conv_id, "Calling stream_prompt()");
    let (mut stream, user_message) = stream_prompt(
        &agent,
        &history,
        user_contents,
        Some(approval_rx),
        Some(resolution_rx),
        max_agent_turns,
    )
    .await?;

    // 4. Optionally add user message to conversation model
    if add_user_message_to_model {
        cx.update_global::<ConversationsStore, _>(|store, _cx| {
            if let Some(conv) = store.get_conversation_mut(&conv_id) {
                conv.add_user_message_with_attachments(user_message, attachment_paths);
            }
        })
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    }

    // 5. Stream processing loop
    debug!(conv_id = %conv_id, "Entering stream processing loop");
    use futures::StreamExt;

    while let Some(chunk_result) = stream.next().await {
        // Check cancellation token before processing
        if cancel_flag.load(Ordering::Relaxed) {
            debug!(conv_id = %conv_id, "Stream cancelled via cancellation token");
            break;
        }

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
    history: &[rig::completion::Message],
    attachment_paths: &[Vec<PathBuf>],
    supports_images: bool,
    supports_pdf: bool,
) -> Vec<PathBuf> {
    if !supports_images && !supports_pdf {
        return Vec::new();
    }
    for (i, msg) in history.iter().enumerate().rev() {
        if matches!(msg, rig::completion::Message::Assistant { .. })
            && let Some(att_paths) = attachment_paths.get(i)
            && !att_paths.is_empty()
        {
            return att_paths
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
        "pdf" => Ok(rig::message::UserContent::document(
            b64,
            Some(rig::completion::message::DocumentMediaType::PDF),
        )),
        _ => Err(anyhow::anyhow!("Unsupported file type: {}", ext)),
    }
}

#[cfg(test)]
mod tests {
    // Re-import standard #[test] to shadow gpui::test from `use gpui::*`
    use core::prelude::rust_2021::test;

    use super::*;
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

    #[test]
    fn select_attachments_no_assistant_messages() {
        let history = vec![user_msg("hello")];
        let attachment_paths = vec![vec![]];
        let result = select_recent_assistant_attachments(&history, &attachment_paths, true, true);
        assert!(result.is_empty());
    }

    #[test]
    fn select_attachments_returns_image_paths() {
        let history = vec![user_msg("hi"), assistant_msg("here's a chart")];
        let attachment_paths = vec![vec![], vec![PathBuf::from("/tmp/chart.png")]];
        let result = select_recent_assistant_attachments(&history, &attachment_paths, true, false);
        assert_eq!(result, vec![PathBuf::from("/tmp/chart.png")]);
    }

    #[test]
    fn select_attachments_filters_pdf_when_unsupported() {
        let history = vec![user_msg("hi"), assistant_msg("report")];
        let attachment_paths = vec![
            vec![],
            vec![
                PathBuf::from("/tmp/chart.png"),
                PathBuf::from("/tmp/report.pdf"),
            ],
        ];
        // images supported, pdf not
        let result = select_recent_assistant_attachments(&history, &attachment_paths, true, false);
        assert_eq!(result, vec![PathBuf::from("/tmp/chart.png")]);
    }

    #[test]
    fn select_attachments_filters_images_when_unsupported() {
        let history = vec![user_msg("hi"), assistant_msg("report")];
        let attachment_paths = vec![
            vec![],
            vec![
                PathBuf::from("/tmp/chart.png"),
                PathBuf::from("/tmp/report.pdf"),
            ],
        ];
        // pdf supported, images not
        let result = select_recent_assistant_attachments(&history, &attachment_paths, false, true);
        assert_eq!(result, vec![PathBuf::from("/tmp/report.pdf")]);
    }

    #[test]
    fn select_attachments_returns_most_recent_only() {
        let history = vec![
            user_msg("first"),
            assistant_msg("old chart"),
            user_msg("second"),
            assistant_msg("new chart"),
        ];
        let attachment_paths = vec![
            vec![],
            vec![PathBuf::from("/tmp/old.png")],
            vec![],
            vec![PathBuf::from("/tmp/new.png")],
        ];
        let result = select_recent_assistant_attachments(&history, &attachment_paths, true, true);
        assert_eq!(result, vec![PathBuf::from("/tmp/new.png")]);
    }

    #[test]
    fn select_attachments_skips_assistant_without_attachments() {
        // Most recent assistant has no attachments, but an earlier one does
        let history = vec![
            user_msg("first"),
            assistant_msg("has chart"),
            user_msg("second"),
            assistant_msg("no chart"),
        ];
        let attachment_paths = vec![
            vec![],
            vec![PathBuf::from("/tmp/old.png")],
            vec![],
            vec![], // most recent assistant has empty attachments
        ];
        let result = select_recent_assistant_attachments(&history, &attachment_paths, true, true);
        // Should skip the empty one and find the older one
        assert_eq!(result, vec![PathBuf::from("/tmp/old.png")]);
    }

    #[test]
    fn select_attachments_no_capability_returns_empty() {
        let history = vec![user_msg("hi"), assistant_msg("chart")];
        let attachment_paths = vec![vec![], vec![PathBuf::from("/tmp/chart.png")]];
        let result = select_recent_assistant_attachments(&history, &attachment_paths, false, false);
        assert!(result.is_empty());
    }

    #[test]
    fn select_attachments_mismatched_lengths_no_panic() {
        // attachment_paths shorter than history
        let history = vec![user_msg("hi"), assistant_msg("chart")];
        let attachment_paths = vec![vec![]]; // only 1 entry for 2 messages
        let result = select_recent_assistant_attachments(&history, &attachment_paths, true, true);
        assert!(result.is_empty());
    }

    #[test]
    fn select_attachments_pdf_case_insensitive() {
        let history = vec![user_msg("hi"), assistant_msg("report")];
        let attachment_paths = vec![vec![], vec![PathBuf::from("/tmp/report.PDF")]];
        let result = select_recent_assistant_attachments(&history, &attachment_paths, false, true);
        assert_eq!(result, vec![PathBuf::from("/tmp/report.PDF")]);
    }
}
