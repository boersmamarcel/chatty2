use gpui::*;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;
use tracing::{debug, error, info, warn};

use crate::chatty::factories::AgentClient;
use crate::chatty::models::token_usage::TokenUsage;
use crate::chatty::models::{
    Conversation, ConversationsStore, GlobalStreamManager, MessageFeedback, StreamChunk,
    StreamManagerEvent, StreamStatus,
};
use crate::chatty::repositories::{
    ConversationData, ConversationJsonRepository, ConversationRepository,
};
use crate::chatty::services::{generate_title, stream_prompt};
use crate::chatty::views::chat_input::{ChatInputEvent, ChatInputState};
use crate::chatty::views::chat_view::ChatViewEvent;
use crate::chatty::views::sidebar_view::SidebarEvent;
use crate::chatty::views::{ChatView, SidebarView};
use crate::settings::models::execution_settings::ExecutionSettingsModel;
use crate::settings::models::models_store::ModelsModel;
use crate::settings::models::providers_store::ProviderModel;
use crate::settings::models::{GlobalMcpNotifier, McpNotifier, McpNotifierEvent};

/// Global state to hold the main ChattyApp entity
#[derive(Default)]
pub struct GlobalChattyApp {
    pub entity: Option<WeakEntity<ChattyApp>>,
}

impl Global for GlobalChattyApp {}

pub struct ChattyApp {
    pub chat_view: Entity<ChatView>,
    pub sidebar_view: Entity<SidebarView>,
    conversation_repo: Arc<dyn ConversationRepository>,
    is_ready: bool,
    /// Held while a conversation is being created; prevents concurrent creations.
    /// Automatically dropped (and thus "cleared") when the task completes.
    active_create_task: Option<Task<anyhow::Result<String>>>,
    /// Keeps the McpNotifier entity alive for the app's lifetime so that
    /// GlobalMcpNotifier's WeakEntity remains upgradeable.
    _mcp_notifier: Entity<McpNotifier>,
}

impl ChattyApp {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Initialize global conversations model if not already done
        if !cx.has_global::<ConversationsStore>() {
            cx.set_global(ConversationsStore::new());
        }

        // Create repository
        let conversation_repo: Arc<dyn ConversationRepository> = Arc::new(
            ConversationJsonRepository::new().expect("Failed to create conversation repository"),
        );

        // Create views
        let chat_view = cx.new(|cx| ChatView::new(window, cx));
        let sidebar_view = cx.new(|_cx| SidebarView::new());

        // Create the MCP notifier and keep the strong entity alive in ChattyApp
        // so GlobalMcpNotifier's WeakEntity remains upgradeable for the app's lifetime.
        let mcp_notifier = cx.new(|_cx| McpNotifier::new());
        cx.set_global(GlobalMcpNotifier {
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
    /// 3. McpNotifier emits McpNotifierEvent → ChattyApp handles
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
                SidebarEvent::ToggleCollapsed(collapsed) => {
                    // Optional: Could save collapsed state to settings here
                    debug!(collapsed = collapsed, "Sidebar toggled");
                }
                SidebarEvent::LoadMore => {
                    let sidebar = app.sidebar_view.clone();
                    sidebar.update(cx, |sidebar, cx| {
                        let store = cx.global::<ConversationsStore>();
                        let total = store.count();
                        let convs = store
                            .list_recent(sidebar.visible_limit())
                            .iter()
                            .map(|c| {
                                (
                                    c.id().to_string(),
                                    c.title().to_string(),
                                    Some(c.token_usage().total_estimated_cost_usd),
                                )
                            })
                            .collect::<Vec<_>>();
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
            },
        )
        .detach();

        // SUBSCRIPTION 3: McpNotifier events — rebuild agent when MCP servers change
        if let Some(weak_notifier) = cx
            .try_global::<GlobalMcpNotifier>()
            .and_then(|g| g.entity.clone())
            && let Some(notifier) = weak_notifier.upgrade()
        {
            cx.subscribe(
                &notifier,
                |this, _notifier, event: &McpNotifierEvent, cx| {
                    if matches!(event, McpNotifierEvent::ServersUpdated) {
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
    ) -> anyhow::Result<Conversation> {
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
            Some(exec_settings.clone()),
            Some(pending_approvals),
            Some(pending_write_approvals),
        )
        .await
    }

    /// Load all conversations from disk
    fn load_conversations(&self, cx: &mut Context<Self>) {
        let repo = self.conversation_repo.clone();
        let sidebar = self.sidebar_view.clone();
        let chat_view = self.chat_view.clone();

        cx.spawn(async move |weak, cx| {
            match repo.load_all().await {
                Ok(conversation_data) => {
                    info!(count = conversation_data.len(), "Loaded conversation files");

                    // Get global stores (need to access them in async context)
                    let models_result =
                        cx.update_global::<ModelsModel, _>(|models, _| models.clone());
                    let providers_result =
                        cx.update_global::<ProviderModel, _>(|providers, _| providers.clone());
                    let mcp_service_result =
                        cx.update_global::<crate::chatty::services::McpService, _>(|svc, _| svc.clone());
                    let exec_settings_result =
                        cx.update_global::<crate::settings::models::ExecutionSettingsModel, _>(|settings, _| settings.clone());
                    let pending_approvals_result =
                        cx.update_global::<crate::chatty::models::ExecutionApprovalStore, _>(|store, _| store.get_pending_approvals());
                    let pending_write_approvals_result =
                        cx.update_global::<crate::chatty::models::WriteApprovalStore, _>(|store, _| store.get_pending_approvals());

                    match (models_result, providers_result, mcp_service_result, exec_settings_result, pending_approvals_result, pending_write_approvals_result) {
                        (Ok(models), Ok(providers), Ok(mcp_service), Ok(exec_settings), Ok(pending_approvals), Ok(pending_write_approvals)) => {
                            let mut restored_count = 0;
                            let mut failed_count = 0;

                            // Reconstruct each conversation
                            for data in conversation_data {
                                let conv_id = data.id.clone();

                                match Self::restore_conversation_from_data(
                                    data, &models, &providers, &mcp_service, &exec_settings, pending_approvals.clone(), pending_write_approvals.clone(),
                                )
                                .await
                                {
                                    Ok(conversation) => {
                                        // Add to global store
                                        cx.update_global::<ConversationsStore, _>(|store, _cx| {
                                            store.add_conversation(conversation);
                                        })
                                        .map_err(|e| warn!(error = ?e, "Failed to add restored conversation to store"))
                                        .ok();

                                        restored_count += 1;
                                        info!(conv_id = %conv_id, "Restored conversation");
                                    }
                                    Err(e) => {
                                        failed_count += 1;
                                        warn!(conv_id = %conv_id, error = ?e, "Failed to restore conversation");
                                    }
                                }
                            }

                            info!(restored = restored_count, failed = failed_count, "Conversation load summary");

                            // Clear the active conversation in the store
                            // This is necessary because add_conversation() auto-sets the first one as active
                            // We want no active conversation so the first message creates a NEW conversation
                            cx.update_global::<ConversationsStore, _>(|store, _| {
                                debug!(active_before = ?store.active_id(), "Clearing active conversation after load");
                                store.clear_active();
                                debug!("Active conversation cleared");
                            }).map_err(|e| warn!(error = ?e, "Failed to clear active conversation"))
                            .ok();

                            // Update sidebar with recent conversations (OPTIMIZATION: only top N)
                            sidebar
                                .update(cx, |sidebar, cx| {
                                    let store = cx.global::<ConversationsStore>();
                                    let total = store.count();
                                    let convs = store
                                        .list_recent(sidebar.visible_limit())
                                        .iter()
                                        .map(|c| (c.id().to_string(), c.title().to_string(), Some(c.token_usage().total_estimated_cost_usd)))
                                        .collect::<Vec<_>>();
                                    debug!(conv_count = convs.len(), total = total, "Loaded conversations, updating sidebar");
                                    sidebar.set_conversations(convs, cx);
                                    sidebar.set_total_count(total);

                                    // Don't set any conversation as active on startup
                                    // This ensures the first message creates a NEW conversation
                                    sidebar.set_active_conversation(None, cx);
                                })
                                .map_err(|e| warn!(error = ?e, "Failed to update sidebar after load"))
                                .ok();

                            // Don't set any conversation as active in the store or chat view
                            // This ensures when the user sends the first message, a new conversation is created
                            chat_view
                                .update(cx, |view, cx| {
                                    view.set_conversation_id(String::new(), cx);
                                    view.clear_messages(cx);
                                    cx.notify();
                                })
                                .map_err(|e| warn!(error = ?e, "Failed to clear chat view on startup"))
                                .ok();

                            // If no conversations existed on disk, create the first one now.
                            // This is the only place where an initial conversation should be
                            // created — after we've confirmed disk has nothing, not before.
                            if restored_count == 0 {
                                info!("No conversations on disk, creating initial conversation");
                                if let Some(app) = weak.upgrade() {
                                    let task_result =
                                        app.update(cx, |app, cx| app.create_new_conversation(cx));
                                    if let Ok(task) = task_result {
                                        let _ = task.await;
                                    }
                                    // Mark app as ready after initial conversation is created
                                    app.update(cx, |app, cx| {
                                        app.is_ready = true;
                                        info!("App is now ready (initial conversation created)");
                                        cx.notify();
                                    })
                                    .map_err(|e| warn!(error = ?e, "Failed to mark app ready after initial conversation"))
                                    .ok();
                                }
                            } else {
                                // Mark app as ready
                                if let Some(app) = weak.upgrade() {
                                    let _: Result<(), _> = app.update(cx, |app, cx| {
                                        app.is_ready = true;
                                        info!("App is now ready (conversations loaded)");
                                        cx.notify();
                                    });
                                }
                            }
                        }
                        _ => {
                            error!("Failed to access global stores");
                        }
                    }
                }
                Err(e) => {
                    error!(error = ?e, "Failed to load conversation files");
                    // Still create an initial conversation so the app is usable
                    if let Some(app) = weak.upgrade() {
                        let task_result =
                            app.update(cx, |app, cx| app.create_new_conversation(cx));
                        if let Ok(task) = task_result {
                            let _ = task.await;
                        }
                        // Mark app as ready so messages can be sent despite load error
                        app.update(cx, |app, cx| {
                            app.is_ready = true;
                            info!("App is now ready (started after load error)");
                            cx.notify();
                        })
                        .map_err(|warn_e| warn!(error = ?warn_e, "Failed to mark app ready after load error"))
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

                // Update sidebar immediately with the new conversation entry
                sidebar.update(cx, |sidebar, cx| {
                    let store = cx.global::<ConversationsStore>();
                    let total = store.count();
                    let mut convs: Vec<(String, String, Option<f64>)> = store
                        .list_recent(sidebar.visible_limit())
                        .iter()
                        .map(|c| {
                            (
                                c.id().to_string(),
                                c.title().to_string(),
                                Some(c.token_usage().total_estimated_cost_usd),
                            )
                        })
                        .collect();
                    sidebar.set_total_count(total);
                    // Prepend the new conversation so it appears at the top
                    convs.insert(0, (conv_id.clone(), title.clone(), Some(0.0)));
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

                    // Get execution settings and approval stores for tools
                    let (exec_settings, pending_approvals, pending_write_approvals) =
                        cx.update(|cx| {
                            let settings = cx
                                .global::<crate::settings::models::ExecutionSettingsModel>()
                                .clone();
                            let approvals = cx
                                .global::<crate::chatty::models::ExecutionApprovalStore>()
                                .get_pending_approvals();
                            let write_approvals = cx
                                .global::<crate::chatty::models::WriteApprovalStore>()
                                .get_pending_approvals();
                            (Some(settings), Some(approvals), Some(write_approvals))
                        })?;

                    let conversation = Conversation::new(
                        conv_id.clone(),
                        title.clone(),
                        &model_config,
                        &provider_config,
                        mcp_tools,
                        exec_settings,
                        pending_approvals,
                        pending_write_approvals,
                    )
                    .await?;

                    // PHASE 3: Add to global store and refresh sidebar with real data
                    cx.update_global::<ConversationsStore, _>(|store, _cx| {
                        store.add_conversation(conversation);
                        store.set_active(conv_id.clone());
                    })?;

                    // Refresh sidebar with actual store data (replaces the placeholder)
                    sidebar.update(cx, |sidebar, cx| {
                        let store = cx.global::<ConversationsStore>();
                        let total = store.count();
                        let convs = store
                            .list_recent(sidebar.visible_limit())
                            .iter()
                            .map(|c| {
                                (
                                    c.id().to_string(),
                                    c.title().to_string(),
                                    Some(c.token_usage().total_estimated_cost_usd),
                                )
                            })
                            .collect::<Vec<_>>();
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
                        created_at: now,
                        updated_at: now,
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

    /// Load a conversation by ID
    fn load_conversation(&mut self, id: &str, cx: &mut Context<Self>) {
        // No need to manually save streaming content - it's already in the Conversation model

        // Set active in store
        let conv_id = id.to_string();
        let chat_view = self.chat_view.clone();
        let sidebar = self.sidebar_view.clone();

        // Update sidebar active state
        sidebar.update(cx, |sidebar, cx| {
            sidebar.set_active_conversation(Some(conv_id.clone()), cx);
        });

        // OPTIMIZATION: Set active and extract only minimal data (model_id, streaming_content)
        // We'll access history/traces/attachments by reference later to avoid cloning
        let minimal_data = cx.update_global::<ConversationsStore, _>(|store, _cx| {
            store.set_active(id.to_string());
            store.get_conversation(id).map(|conv| {
                (
                    conv.model_id().to_string(),
                    conv.streaming_message().cloned(),
                )
            })
        });

        if let Some((model_id, streaming_content)) = minimal_data {
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
                            conv.system_traces().to_vec(),  // Clones JSON values, not deserialized traces
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
                });

                // Restore in-progress streaming message from Conversation model if it exists
                // This must happen AFTER setting the streaming state
                if has_active_stream {
                    if let Some(content) = streaming_content {
                        debug!(conv_id = %conv_id, content_len = content.len(),
                               "Restoring streaming message content from Conversation model");
                        // Start a new streaming message and populate it with content from model
                        view.start_assistant_message(cx);
                        view.append_assistant_text(&content, cx);
                    } else {
                        // Stream active but no content yet - show placeholder
                        debug!(conv_id = %conv_id, "Stream active but no content yet, starting placeholder");
                        view.start_assistant_message(cx);
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
        let conversations = store.list_recent(usize::MAX);

        if conversations.is_empty() {
            return;
        }

        let conv_ids: Vec<String> = conversations.iter().map(|c| c.id().to_string()).collect();

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
                    .map_err(|e| warn!(error = ?e, "Failed to clear active_create_task"))
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

        let model_id = cx
            .global::<ConversationsStore>()
            .get_conversation(&conv_id)
            .map(|c| c.model_id().to_string());

        let Some(model_id) = model_id else { return };

        let models = cx.global::<ModelsModel>();
        let providers = cx.global::<ProviderModel>();

        let model_config = models.get_model(&model_id).cloned();
        let provider_config = model_config.as_ref().and_then(|mc| {
            providers
                .providers()
                .iter()
                .find(|p| p.provider_type == mc.provider_type)
                .cloned()
        });

        let (Some(model_config), Some(provider_config)) = (model_config, provider_config) else {
            error!(
                model_id = %model_id,
                "Could not find model or provider config for agent rebuild"
            );
            return;
        };

        info!(
            conv_id = %conv_id,
            model_id = %model_id,
            "Rebuilding conversation agent after tool set change"
        );

        cx.spawn(async move |_weak, cx| -> anyhow::Result<()> {
            let mcp_service = cx
                .update(|cx| cx.global::<crate::chatty::services::McpService>().clone())
                .map_err(|e| anyhow::anyhow!(e.to_string()))?;

            let mcp_tools = mcp_service.get_all_tools_with_sinks().await.ok();
            let mcp_tools =
                mcp_tools.and_then(|tools| if tools.is_empty() { None } else { Some(tools) });

            let mcp_server_count = mcp_tools.as_ref().map(|t| t.len()).unwrap_or(0);
            let mcp_tool_count: usize = mcp_tools
                .as_ref()
                .map(|t| t.iter().map(|(_, tools, _)| tools.len()).sum())
                .unwrap_or(0);
            info!(
                conv_id = %conv_id,
                mcp_server_count,
                mcp_tool_count,
                "Rebuilding agent with fresh MCP tools"
            );

            let (exec_settings, pending_approvals, pending_write_approvals, pending_artifacts, shell_session) = cx
                .update(|cx| {
                    let settings = cx
                        .global::<crate::settings::models::ExecutionSettingsModel>()
                        .clone();
                    let approvals = cx
                        .global::<crate::chatty::models::ExecutionApprovalStore>()
                        .get_pending_approvals();
                    let write_approvals = cx
                        .global::<crate::chatty::models::WriteApprovalStore>()
                        .get_pending_approvals();
                    let conv = cx
                        .global::<ConversationsStore>()
                        .get_conversation(&conv_id);
                    let artifacts = conv.map(|c| c.pending_artifacts());
                    // Drop the existing shell session if network_isolation changed — it was
                    // spawned with the old setting and cannot be reconfigured in place.
                    // Passing None lets the factory create a fresh session with the new setting.
                    let session = conv.and_then(|c| c.shell_session()).and_then(|s| {
                        if s.network_isolation() == settings.network_isolation {
                            Some(s)
                        } else {
                            info!(
                                conv_id = %conv_id,
                                new_isolation = settings.network_isolation,
                                "Network isolation changed — replacing shell session"
                            );
                            None
                        }
                    });
                    (Some(settings), Some(approvals), Some(write_approvals), artifacts, session)
                })
                .map_err(|e| anyhow::anyhow!(e.to_string()))?;

            // Factory creates shell session on-demand if not provided
            let (new_agent, new_shell_session) = AgentClient::from_model_config_with_tools(
                &model_config,
                &provider_config,
                mcp_tools,
                exec_settings,
                pending_approvals,
                pending_write_approvals,
                pending_artifacts,
                shell_session,
            )
            .await?;

            cx.update_global::<ConversationsStore, _>(|store, _cx| {
                if let Some(conv) = store.get_conversation_mut(&conv_id) {
                    conv.set_agent(new_agent, model_config.id.clone());
                    // Always store the new shell session — the factory either reused the
                    // existing one or created a fresh one (e.g. after a network_isolation change).
                    if new_shell_session.is_some() {
                        conv.set_shell_session(new_shell_session);
                    }
                    info!(conv_id = %conv_id, "Agent successfully rebuilt with updated tool set");
                } else {
                    warn!(conv_id = %conv_id, "Conversation not found during agent rebuild — skipping");
                }
            })
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;

            Ok(())
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
                        let mcp_tools = mcp_service.get_all_tools_with_sinks().await.ok();

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
                        ) = cx
                            .update(|cx| {
                                let settings = cx
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
                                let artifacts = conv.map(|c| c.pending_artifacts());
                                let session = conv.and_then(|c| c.shell_session());
                                (
                                    Some(settings),
                                    Some(approvals),
                                    Some(write_approvals),
                                    artifacts,
                                    session,
                                )
                            })
                            .map_err(|e| anyhow::anyhow!(e.to_string()))?;

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
                            )
                            .await?;

                        // Update the conversation's agent synchronously
                        cx.update_global::<ConversationsStore, _>(|store, _cx| {
                            if let Some(conv) = store.get_conversation_mut(&conv_id) {
                                debug!("Updating conversation model");
                                conv.set_agent(new_agent, model_config.id.clone());
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
                                        created_at: conv
                                            .created_at()
                                            .duration_since(SystemTime::UNIX_EPOCH)
                                            .unwrap()
                                            .as_secs()
                                            as i64,
                                        updated_at: now,
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
        } else {
            error!("No active conversation");
        }
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
            let convs = store
                .list_recent(sidebar.visible_limit())
                .iter()
                .map(|c| {
                    (
                        c.id().to_string(),
                        c.title().to_string(),
                        Some(c.token_usage().total_estimated_cost_usd),
                    )
                })
                .collect::<Vec<_>>();
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
                                // Promote the pending stream to the real conversation ID
                                if let Some(ref sm) = stream_manager {
                                    sm.update(cx, |mgr, _cx| {
                                        mgr.promote_pending(&id);
                                    })
                                    .map_err(|e| warn!(error = ?e, "Failed to promote pending stream"))
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
                                    .map_err(|e| warn!(error = ?e, "Failed to cancel pending stream on error"))
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
                }).map_err(|e| warn!(error = ?e, "Failed to refresh sidebar after creating conversation"))
                .ok();

                // Extract agent, history, model_id, and capabilities synchronously
                let (agent, history, _model_id, provider_supports_pdf, provider_supports_images) = cx
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
                            ))
                        } else {
                            Err(anyhow::anyhow!(
                                "Could not find conversation after creation/lookup"
                            ))
                        }
                    })
                    .map_err(|e| anyhow::anyhow!(e.to_string()))??;

                // PHASE 3: Create approval notification channels
                let (approval_tx, approval_rx) = tokio::sync::mpsc::unbounded_channel();
                let (resolution_tx, resolution_rx) = tokio::sync::mpsc::unbounded_channel();

                // Set up global notifier for shell tools to use
                crate::chatty::models::execution_approval_store::set_global_approval_notifier(approval_tx.clone());

                // Update global store with notifiers for this conversation
                // IMPORTANT: Use update_global to modify existing store, not set_global which replaces it
                // Replacing the store would break the pending_requests HashMap connection
                cx.update_global::<crate::chatty::models::execution_approval_store::ExecutionApprovalStore, _>(
                    |store, _cx| {
                        store.set_notifiers(approval_tx, resolution_tx);
                    }
                )
                .map_err(|e| warn!(error = ?e, "Failed to update approval store with notifiers"))
                .ok();

                // Prepare message contents with attachment filtering
                debug!("Calling stream_prompt()");
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
                let max_agent_turns = cx
                    .update(|cx| {
                        cx.global::<ExecutionSettingsModel>()
                            .max_agent_turns as usize
                    })
                    .unwrap_or(10);

                let (mut stream, user_message) =
                    stream_prompt(&agent, &history, contents, Some(approval_rx), Some(resolution_rx), max_agent_turns).await?;

                // Update history synchronously with user message
                cx.update_global::<ConversationsStore, _>(|store, _cx| {
                    if let Some(conv) = store.get_conversation_mut(&conv_id) {
                        conv.add_user_message_with_attachments(user_message, attachments.clone());
                    }
                })
                .map_err(|e| anyhow::anyhow!(e.to_string()))?;

                debug!("Got stream, starting to process");
                use futures::StreamExt;
                let mut chunk_count = 0;

                // PHASE 4: Process LLM response stream
                debug!("Entering stream processing loop");
                while let Some(chunk_result) = stream.next().await {
                    // Check cancellation token before processing
                    if cancel_flag_for_loop.load(Ordering::Relaxed) {
                        debug!("Stream cancelled via cancellation token");
                        break;
                    }
                    chunk_count += 1;
                    debug!(chunk_num = chunk_count, chunk = ?chunk_result, "Processing stream chunk");

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
                            debug!("Received Done chunk");
                            break;
                        }
                        Ok(StreamChunk::Error(ref err)) => {
                            error!(error = %err, "Stream error");

                            // Detect authentication errors (401/Unauthorized)
                            if err.contains("401") || err.contains("Unauthorized") {
                                tracing::warn!("Detected Azure auth error - token likely expired");
                                if let Some(cache) = cx.update(|cx| {
                                    cx.try_global::<crate::chatty::auth::AzureTokenCache>().cloned()
                                }).ok().flatten() {
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
                            error!(error = %e, "Stream error");
                        }
                    }

                    // Forward ALL chunks to StreamManager (emits events for UI subscription)
                    match chunk_result {
                        Ok(chunk) => {
                            let is_break = matches!(chunk, StreamChunk::Done | StreamChunk::Error(_));
                            if let Some(ref sm) = stream_manager {
                                sm.update(cx, |sm, cx| sm.handle_chunk(&conv_id, chunk, cx))
                                    .map_err(|e| warn!(error = ?e, "Failed to forward chunk to StreamManager"))
                                    .ok();
                            }
                            if is_break {
                                break;
                            }
                        }
                        Err(e) => {
                            if let Some(ref sm) = stream_manager {
                                sm.update(cx, |sm, cx| {
                                    sm.handle_chunk(&conv_id, StreamChunk::Error(e.to_string()), cx);
                                })
                                .map_err(|e| warn!(error = ?e, "Failed to forward error to StreamManager"))
                                .ok();
                            }
                            break;
                        }
                    }
                }

                // PHASE 5: Extract trace and finalize via StreamManager
                // StreamManager emits StreamEnded(Completed) which triggers
                // UI finalization, conversation model update, title gen, token usage, and persistence
                debug!("Stream loop finished, finalizing via StreamManager");

                let trace_json = chat_view
                    .update(cx, |view, _cx| view.extract_current_trace())
                    .map_err(|e| anyhow::anyhow!(e.to_string()))?
                    .and_then(|trace| serde_json::to_value(&trace).ok());

                if let Some(ref sm) = stream_manager {
                    sm.update(cx, |sm, cx| {
                        sm.set_trace(&conv_id, trace_json);
                        sm.finalize_stream(&conv_id, cx);
                    })
                    .map_err(|e| warn!(error = ?e, "Failed to finalize stream in StreamManager"))
                    .ok();
                }

                Ok(())
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
                        self.finalize_completed_stream(
                            conversation_id,
                            *token_usage,
                            trace_json.clone(),
                            cx,
                        );

                        // Check for artifacts queued by AddAttachmentTool.
                        // If the stream manager had them, use those; otherwise fall back
                        // to checking the conversation directly (for pending-promoted streams).
                        let artifacts = pending_artifacts.clone().or_else(|| {
                            cx.try_global::<ConversationsStore>()
                                .and_then(|store| store.get_conversation(conversation_id))
                                .and_then(|conv| {
                                    conv.pending_artifacts()
                                        .lock()
                                        .ok()
                                        .map(|mut v| v.drain(..).collect::<Vec<_>>())
                                })
                                .filter(|v| !v.is_empty())
                        });

                        if let Some(artifact_paths) = artifacts
                            && !artifact_paths.is_empty()
                        {
                            debug!(
                                count = artifact_paths.len(),
                                "Sending follow-up with queued artifacts as multimodal content"
                            );
                            let follow_up_text =
                                "Here are the attached files for your analysis.".to_string();
                            let app_weak = cx.entity().downgrade();
                            cx.defer(move |cx| {
                                if let Some(app) = app_weak.upgrade() {
                                    app.update(cx, |app, cx| {
                                        app.send_message(follow_up_text, artifact_paths, cx);
                                    });
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

                // Clear streaming message from Conversation model
                cx.update_global::<ConversationsStore, _>(|store, _cx| {
                    if let Some(conv) = store.get_conversation_mut(conversation_id) {
                        conv.set_streaming_message(None);
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

        // Extract trace before stopping (only if conversation is displayed)
        let trace_json = self.chat_view.update(cx, |view, _cx| {
            view.extract_current_trace()
                .and_then(|trace| serde_json::to_value(&trace).ok())
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
        //    finalize in conversation model, and check if title gen needed
        let should_generate_title =
            cx.update_global::<ConversationsStore, _>(|store, _cx| {
                if let Some(conv) = store.get_conversation_mut(&conv_id) {
                    let response_text = conv
                        .streaming_message()
                        .cloned()
                        .unwrap_or_default();
                    conv.finalize_response(response_text);
                    conv.add_trace(trace_json);
                    let msg_count = conv.message_count();
                    let should_gen = msg_count == 2 && conv.title() == "New Chat";
                    debug!(conv_id = %conv_id, msg_count, should_gen, "Response finalized in conversation");
                    should_gen
                } else {
                    error!(conv_id = %conv_id, "Could not find conversation to finalize");
                    false
                }
            });

        // 3. Process token usage
        if let Some((input_tokens, output_tokens)) = token_usage {
            debug!(input_tokens, output_tokens, "Processing token usage");

            let model_id_opt = cx.update_global::<ConversationsStore, _>(|store, _cx| {
                store
                    .get_conversation(&conv_id)
                    .map(|conv| conv.model_id().to_string())
            });

            if let Some(model_id) = model_id_opt {
                let pricing = cx.update_global::<ModelsModel, _>(|models, _cx| {
                    models.get_model(&model_id).and_then(|model| {
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
                    let mut usage = TokenUsage::new(input_tokens, output_tokens);
                    usage.calculate_cost(cost_per_million_input, cost_per_million_output);

                    cx.update_global::<ConversationsStore, _>(|store, _cx| {
                        if let Some(conv) = store.get_conversation_mut(&conv_id) {
                            conv.add_token_usage(usage);
                        }
                    });
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
                                    conv.set_title(new_title);
                                }
                            })
                            .map_err(|e| warn!(error = ?e, "Failed to update conversation title"))
                            .ok();

                            // Update sidebar with new title
                            sidebar_for_title
                                .update(cx, |sidebar, cx| {
                                    let store = cx.global::<ConversationsStore>();
                                    let total = store.count();
                                    let convs = store
                                        .list_recent(sidebar.visible_limit())
                                        .iter()
                                        .map(|c| {
                                            (
                                                c.id().to_string(),
                                                c.title().to_string(),
                                                Some(
                                                    c.token_usage()
                                                        .total_estimated_cost_usd,
                                                ),
                                            )
                                        })
                                        .collect::<Vec<_>>();
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
        cx.update_global::<ConversationsStore, _>(|store, _cx| {
            if let Some(conv) = store.get_conversation_mut(&conv_id) {
                let partial_text = conv.streaming_message().cloned().unwrap_or_default();
                conv.finalize_response(partial_text);
                conv.add_trace(trace_json);
                conv.set_streaming_message(None);
                debug!(conv_id = %conv_id, "Partial response saved to conversation after stop");
            }
        });

        // Persist to disk
        self.persist_conversation(&conv_id, cx);
    }

    /// Refresh the sidebar with the latest conversation list from the store
    fn refresh_sidebar(&self, cx: &mut Context<Self>) {
        self.sidebar_view.update(cx, |sidebar, cx| {
            let store = cx.global::<ConversationsStore>();
            let total = store.count();
            let convs = store
                .list_recent(sidebar.visible_limit())
                .iter()
                .map(|c| {
                    (
                        c.id().to_string(),
                        c.title().to_string(),
                        Some(c.token_usage().total_estimated_cost_usd),
                    )
                })
                .collect::<Vec<_>>();
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

    /// Persist a conversation to disk asynchronously
    fn persist_conversation(&self, conv_id: &str, cx: &mut Context<Self>) {
        let conv_id = conv_id.to_string();
        let repo = self.conversation_repo.clone();

        let conv_data_opt = cx.update_global::<ConversationsStore, _>(|store, _cx| {
            store.get_conversation(&conv_id).and_then(|conv| {
                let history = conv.serialize_history().ok()?;
                let traces = conv.serialize_traces().ok()?;
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
                    created_at: conv
                        .created_at()
                        .duration_since(SystemTime::UNIX_EPOCH)
                        .unwrap()
                        .as_secs() as i64,
                    updated_at: now,
                })
            })
        });

        if let Some(conv_data) = conv_data_opt {
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
        }
    }

    /// Get the chat input state entity
    #[allow(dead_code)]
    pub fn chat_input_state(&self, cx: &App) -> Entity<ChatInputState> {
        self.chat_view.read(cx).chat_input_state().clone()
    }
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
