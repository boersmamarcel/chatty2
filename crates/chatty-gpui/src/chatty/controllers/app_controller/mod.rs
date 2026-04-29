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
use crate::chatty::services::{generate_title, stream_prompt};
use crate::chatty::token_budget::{
    GlobalTokenBudget, check_pressure, compute_snapshot_background, extract_user_message_text,
    gather_snapshot_inputs, summarize_oldest_half,
};
use crate::chatty::tools::LocalModuleAgentSummary;
use crate::chatty::views::chat_input::{ChatInputEvent, ChatInputState, SkillEntry};
use crate::chatty::views::chat_view::ChatViewEvent;
use crate::chatty::views::message_types::{
    ApprovalBlock, ApprovalState, SystemTrace, ThinkingState, ToolCallBlock, ToolCallState,
    ToolSource, TraceItem, friendly_tool_name, is_denial_result,
};
use crate::chatty::views::sidebar_view::SidebarEvent;
use crate::chatty::views::{ChatView, SidebarView};
use crate::settings::models::TokenTrackingSettings;
use crate::settings::models::execution_settings::ExecutionSettingsModel;
use crate::settings::models::models_store::{ModelConfig, ModelsModel};
use crate::settings::models::providers_store::ProviderModel;
use crate::settings::models::training_settings::TrainingSettingsModel;
use crate::settings::models::{AgentConfigEvent, AgentConfigNotifier, GlobalAgentConfigNotifier};
use crate::settings::models::{DiscoveredModulesModel, ModuleLoadStatus};
use chatty_core::factories::agent_factory::AgentBuildContext;

mod conversation_ops;
mod export_ops;
mod message_ops;
mod slash_commands;

/// Collect WASM module agents from the global `DiscoveredModulesModel` and convert them to
/// `LocalModuleAgentSummary` values suitable for the `list_agents` tool.
///
/// Only modules with `agent = true`, a `Loaded` status, and enabled in `ExtensionsModel`
/// are included.
fn collect_module_agents(cx: &App) -> Vec<LocalModuleAgentSummary> {
    let enabled_ids: std::collections::HashSet<&str> = cx
        .try_global::<chatty_core::settings::models::extensions_store::ExtensionsModel>()
        .map(|ext| {
            ext.wasm_module_ids()
                .into_iter()
                .collect::<std::collections::HashSet<_>>()
        })
        .unwrap_or_default();

    cx.try_global::<DiscoveredModulesModel>()
        .map(|model| {
            model
                .modules
                .iter()
                .filter(|m| {
                    m.agent
                        && matches!(
                            m.status,
                            ModuleLoadStatus::Loaded | ModuleLoadStatus::Remote
                        )
                        && enabled_ids.contains(m.name.as_str())
                })
                .map(|m| LocalModuleAgentSummary {
                    name: m.name.clone(),
                    version: m.version.clone(),
                    description: m.description.clone(),
                    tools: m.tools.clone(),
                    supports_a2a: m.a2a,
                    execution_mode: m.execution_mode.clone(),
                })
                .collect()
        })
        .unwrap_or_default()
}

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

    let mcp_tools = chatty_core::services::gather_mcp_tools(&mcp_service).await;

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
                .map(|m| m.models().iter().map(|m| m.id.clone()).collect::<Vec<_>>())
                .unwrap_or_default();
            (agents, model_ids)
        })
        .unwrap_or_default();

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
                skill_service: None,
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
            conv.set_invoke_agent_progress_slot(new_progress_slot);
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
pub type GlobalChattyApp = crate::global_entity::GlobalWeakEntity<ChattyApp>;

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
    /// Tool-call IDs for active invoke_agent calls. These are suppressed from the
    /// chat UI (no ToolCallBlock) and instead visualised via the sub-agent progress
    /// system, identical to the `/agent` slash command.
    active_invoke_agent_ids: std::collections::HashSet<String>,
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
        cx.set_global(GlobalAgentConfigNotifier::new(mcp_notifier.downgrade()));

        let app = Self {
            chat_view,
            sidebar_view,
            conversation_repo,
            is_ready: false,
            active_create_task: None,
            _mcp_notifier: mcp_notifier,
            active_invoke_agent_ids: std::collections::HashSet::new(),
        };

        // Store entity in global state for later access
        let app_weak = cx.entity().downgrade();
        if !cx.has_global::<GlobalChattyApp>() {
            cx.set_global(GlobalChattyApp::new(app_weak));
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
        if let Some(notifier) = cx
            .try_global::<GlobalAgentConfigNotifier>()
            .and_then(|g| g.try_upgrade())
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
        if let Some(manager) = cx.try_global::<GlobalStreamManager>().and_then(|g| g.get()) {
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

        // Load skills for the initial workspace directory
        let workspace_dir = cx
            .try_global::<ExecutionSettingsModel>()
            .and_then(|s| s.workspace_dir.clone())
            .map(PathBuf::from);
        self.refresh_chat_input_skills(workspace_dir.as_deref(), cx);
    }

    /// Synchronously load filesystem skills for `workspace_dir` (and the global skills dir)
    /// and push them into the chat-input picker so they appear in the `/` menu.
    fn refresh_chat_input_skills(&self, workspace_dir: Option<&Path>, cx: &mut Context<Self>) {
        let skill_service = cx
            .try_global::<chatty_core::services::SkillService>()
            .cloned()
            .unwrap_or_else(|| chatty_core::services::SkillService::new(None));

        let workspace_skills_dir = workspace_dir.map(|d| d.join(".claude").join("skills"));

        let raw_skills = skill_service.list_all_skills_sync(workspace_skills_dir.as_deref());

        let entries: Vec<SkillEntry> = raw_skills
            .into_iter()
            .map(|(name, description)| SkillEntry { name, description })
            .collect();

        debug!(
            count = entries.len(),
            "Refreshed skills for slash-command picker"
        );

        let chat_view = self.chat_view.clone();
        chat_view.update(cx, |view, cx| {
            view.chat_input_state().update(cx, |state, cx| {
                state.set_available_skills(entries, cx);
            });
        });
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

    /// Get the chat input state entity
    #[allow(dead_code)]
    pub fn chat_input_state(&self, cx: &App) -> Entity<ChatInputState> {
        self.chat_view.read(cx).chat_input_state().clone()
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
        .unwrap_or_default()
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
            .unwrap_or_default()
            .as_secs() as i64,
        updated_at: now,
        working_dir: conv.working_dir().map(|p| p.to_string_lossy().to_string()),
    })
}

// ── Tool source classification ───────────────────────────────────────────────

/// Classify a built-in tool call by name into a [`ToolSource`] for data-egress badges.
///
/// Internet-facing tools are classified here. Module agent calls (invoke_agent /
/// sub_agent) are classified separately by [`classify_agent_source`].
pub(super) fn classify_tool_source(tool_name: &str) -> ToolSource {
    chatty_core::models::message_types::classify_tool_source(tool_name)
}

/// Classify an agent invocation by agent name into a [`ToolSource`].
///
/// Checks the global [`DiscoveredModulesModel`] for remote WASM modules and the
/// global [`ExtensionsModel`] for non-localhost A2A agents.
pub(super) fn classify_agent_source(agent_name: &str, cx: &App) -> ToolSource {
    use chatty_core::settings::models::extensions_store::ExtensionsModel;

    // Remote WASM module on the Hive runner?
    if let Some(discovered) = cx.try_global::<DiscoveredModulesModel>() {
        if let Some(entry) = discovered.modules.iter().find(|m| m.name == agent_name) {
            if entry.execution_mode == "remote" || entry.execution_mode == "remote_only" {
                return ToolSource::HiveCloud;
            }
        }
    }

    // Remote A2A agent with a non-localhost URL?
    if let Some(extensions) = cx.try_global::<ExtensionsModel>() {
        if let Some((_, cfg, _)) = extensions
            .all_a2a_agents()
            .into_iter()
            .find(|(_, cfg, _)| cfg.name == agent_name)
        {
            let is_local = cfg.url.contains("localhost") || cfg.url.contains("127.0.0.1");
            if !is_local {
                return ToolSource::ExternalService {
                    name: cfg.name.clone(),
                };
            }
        }
    }

    ToolSource::Local
}
