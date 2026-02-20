use gpui::*;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;
use tracing::{debug, error, info, warn};

use crate::chatty::factories::AgentClient;
use crate::chatty::models::token_usage::TokenUsage;
use crate::chatty::models::{Conversation, ConversationsStore, StreamChunk};
use crate::chatty::repositories::{
    ConversationData, ConversationJsonRepository, ConversationRepository,
};
use crate::chatty::services::{generate_title, stream_prompt};
use crate::chatty::views::chat_input::ChatInputState;
use crate::chatty::views::{ChatView, SidebarView};
use crate::settings::models::models_store::ModelsModel;
use crate::settings::models::providers_store::ProviderModel;

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
    /// Maps conversation_id → active stream task
    /// Allows multiple conversations to have concurrent streams
    active_stream_tasks: HashMap<String, Task<anyhow::Result<()>>>,
    /// Held while a conversation is being created; prevents concurrent creations.
    /// Automatically dropped (and thus "cleared") when the task completes.
    active_create_task: Option<Task<anyhow::Result<String>>>,
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

        let app = Self {
            chat_view,
            sidebar_view,
            conversation_repo,
            is_ready: false,
            active_stream_tasks: HashMap::new(),
            active_create_task: None,
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

    /// Set up all callbacks between components
    fn setup_callbacks(&self, cx: &mut Context<Self>) {
        // Setup sidebar callbacks
        let chat_view = self.chat_view.clone();
        let sidebar = self.sidebar_view.clone();

        // Get entity to use in callbacks (avoids window lookup issues)
        let app_entity = cx.entity();

        // New chat callback (guarded against rapid clicks via active_create_task)
        sidebar.update(cx, |sidebar, _cx| {
            let app = app_entity.clone();
            sidebar.set_on_new_chat(move |cx| {
                let app = app.clone();
                app.update(cx, |app, cx| {
                    if app.active_create_task.is_some() {
                        debug!("Already creating a conversation, ignoring duplicate click");
                        return;
                    }
                    let create_task = app.create_new_conversation(cx);
                    // Wrap so the guard auto-clears when the task finishes
                    app.active_create_task = Some(cx.spawn(async move |weak, cx| {
                        let result = create_task.await;
                        if let Some(app) = weak.upgrade() {
                            app.update(cx, |app, _cx| app.active_create_task = None)
                                .map_err(
                                    |e| warn!(error = ?e, "Failed to clear active_create_task"),
                                )
                                .ok();
                        }
                        result
                    }));
                });
            });
        });

        // Settings callback
        sidebar.update(cx, |sidebar, _cx| {
            sidebar.set_on_settings(move |cx| {
                cx.defer(|cx| {
                    use crate::settings::controllers::SettingsView;
                    SettingsView::open_or_focus_settings_window(cx);
                });
            });
        });

        // Select conversation callback
        sidebar.update(cx, |sidebar, _cx| {
            let app = app_entity.clone();
            sidebar.set_on_select_conversation(move |conv_id, cx| {
                let app = app.clone();
                let id = conv_id.to_string();
                app.update(cx, |app, cx| {
                    app.load_conversation(&id, cx);
                });
            });
        });

        // Delete conversation callback
        sidebar.update(cx, |sidebar, _cx| {
            let app = app_entity.clone();
            sidebar.set_on_delete_conversation(move |conv_id, cx| {
                let app = app.clone();
                let id = conv_id.to_string();
                app.update(cx, |app, cx| {
                    app.delete_conversation(&id, cx);
                });
            });
        });

        // Toggle sidebar callback
        sidebar.update(cx, |sidebar, _cx| {
            sidebar.set_on_toggle(move |collapsed, _cx| {
                // Optional: Could save collapsed state to settings here
                debug!(collapsed = collapsed, "Sidebar toggled");
            });
        });

        // Chat input send message callback
        chat_view.update(cx, |view, cx| {
            let input_state = view.chat_input_state().clone();
            let app_for_send = app_entity.clone();
            input_state.update(cx, |state, _cx| {
                debug!("Setting up on_send callback for chat input");
                state.set_on_send(move |message, attachments, cx| {
                    debug!(message = %message, attachment_count = attachments.len(), "on_send callback triggered");
                    let app = app_for_send.clone();
                    let msg = message.clone();
                    let att = attachments.clone();

                    debug!("Calling send_message directly via entity");
                    app.update(cx, |app, cx| {
                        app.send_message(msg, att, cx);
                    });
                });
            });
        });

        // Chat input model change callback
        chat_view.update(cx, |view, cx| {
            let input_state = view.chat_input_state().clone();
            let app_for_model = app_entity.clone();
            input_state.update(cx, |state, _cx| {
                debug!("Setting up on_model_change callback for chat input");
                state.set_on_model_change(move |model_id, cx| {
                    debug!(model_id = %model_id, "on_model_change callback triggered");
                    let app = app_for_model.clone();
                    let mid = model_id.clone();

                    app.update(cx, |app, cx| {
                        // Defer capability update to avoid re-entering ChatInputState
                        let chat_view = app.chat_view.clone();
                        let mid_for_defer = mid.clone();
                        cx.defer(move |cx| {
                            let capabilities = cx
                                .global::<ModelsModel>()
                                .get_model(&mid_for_defer)
                                .map(|m| (m.supports_images, m.supports_pdf))
                                .unwrap_or((false, false));

                            chat_view.update(cx, |view, cx| {
                                view.chat_input_state().update(cx, |state, _cx| {
                                    state.set_capabilities(capabilities.0, capabilities.1);
                                });
                            });
                        });

                        app.change_conversation_model(mid, cx);
                    });
                });
            });
        });

        // Chat input stop stream callback
        chat_view.update(cx, |view, cx| {
            let input_state = view.chat_input_state().clone();
            let app_for_stop = app_entity.clone();
            input_state.update(cx, |state, _cx| {
                debug!("Setting up on_stop callback for chat input");
                state.set_on_stop(move |cx| {
                    debug!("on_stop callback triggered");
                    let app = app_for_stop.clone();

                    app.update(cx, |app, cx| {
                        // stop_stream now handles clearing the stream from HashMap
                        // and updating UI state, so no need for separate clear_streaming_state call
                        app.stop_stream(cx);
                    });
                });
            });
        });
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

                            // Update sidebar with all conversations
                            sidebar
                                .update(cx, |sidebar, cx| {
                                    let convs = cx
                                        .global::<ConversationsStore>()
                                        .list_all()
                                        .iter()
                                        .map(|c| (c.id().to_string(), c.title().to_string(), Some(c.token_usage().total_estimated_cost_usd)))
                                        .collect::<Vec<_>>();
                                    debug!(conv_count = convs.len(), "Loaded conversations, updating sidebar");
                                    sidebar.set_conversations(convs, cx);

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

                // Cancel any pending stream for conversations without IDs
                if let Some(task) = self.active_stream_tasks.remove("__pending__") {
                    debug!("Cancelling pending stream when creating new conversation");
                    drop(task);
                }

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
                    let mut convs: Vec<(String, String, Option<f64>)> = cx
                        .global::<ConversationsStore>()
                        .list_all()
                        .iter()
                        .map(|c| {
                            (
                                c.id().to_string(),
                                c.title().to_string(),
                                Some(c.token_usage().total_estimated_cost_usd),
                            )
                        })
                        .collect();
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
                        let convs = cx
                            .global::<ConversationsStore>()
                            .list_all()
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

        // Batch: set active + get conversation data + streaming content in single global lookup
        let conversation_data = cx.update_global::<ConversationsStore, _>(|store, _cx| {
            store.set_active(id.to_string());
            store.get_conversation(id).map(|conv| {
                (
                    conv.history().to_vec(),
                    conv.system_traces().to_vec(),
                    conv.attachment_paths().to_vec(),
                    conv.model_id().to_string(),
                    conv.streaming_message().map(|s| s.clone()),
                )
            })
        });

        if let Some((history, traces, attachment_paths, model_id, streaming_content)) =
            conversation_data
        {
            chat_view.update(cx, |view, cx| {
                view.set_conversation_id(conv_id.clone(), cx);

                // Clear attachments from previous conversation
                view.chat_input_state().update(cx, |state, _cx| {
                    state.clear_attachments();
                });

                view.load_history(&history, &traces, &attachment_paths, cx);

                // Check if this conversation has an active stream
                let has_active_stream = self.active_stream_tasks.contains_key(&conv_id);

                // Update the selected model and capabilities in the chat input
                let model_capabilities = cx
                    .global::<ModelsModel>()
                    .get_model(&model_id)
                    .map(|m| (m.supports_images, m.supports_pdf))
                    .unwrap_or((false, false));

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

    /// Change the model for the active conversation
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
                        let (exec_settings, pending_approvals, pending_write_approvals) = cx
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
                                (Some(settings), Some(approvals), Some(write_approvals))
                            })
                            .map_err(|e| anyhow::anyhow!(e.to_string()))?;

                        // Create new agent asynchronously with MCP tools
                        let new_agent = AgentClient::from_model_config_with_tools(
                            &model_config,
                            &provider_config,
                            mcp_tools,
                            exec_settings,
                            pending_approvals,
                            pending_write_approvals,
                        )
                        .await?;

                        // Update the conversation's agent synchronously
                        cx.update_global::<ConversationsStore, _>(|store, _cx| {
                            if let Some(conv) = store.get_conversation_mut(&conv_id) {
                                debug!("Updating conversation model");
                                conv.set_agent(new_agent, model_config.id.clone());
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
            let convs = cx
                .global::<ConversationsStore>()
                .list_all()
                .iter()
                .map(|c| {
                    (
                        c.id().to_string(),
                        c.title().to_string(),
                        Some(c.token_usage().total_estimated_cost_usd),
                    )
                })
                .collect::<Vec<_>>();

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

    /// Send a message to the LLM and stream the response
    ///
    /// This is the main message-handling function with the following phases:
    /// 1. Ensure conversation exists (create if needed)
    /// 2. Update UI with user message
    /// 3. Filter attachments based on provider capabilities
    /// 4. Stream LLM response and update UI incrementally
    /// 5. Finalize response and save to conversation
    /// 6. Generate title for first exchange
    /// 7. Update token usage and persist to disk
    ///
    /// # Note
    /// This function is complex (400+ lines) and could benefit from extraction
    /// of helper functions in future refactoring. The main complexity comes from:
    /// - Async/await with GPUI entity updates
    /// - Stream processing with multiple chunk types
    /// - UI synchronization during streaming
    /// - Title generation and token usage tracking
    fn send_message(&mut self, message: String, attachments: Vec<PathBuf>, cx: &mut Context<Self>) {
        debug!(message = %message, attachment_count = attachments.len(), "send_message called");

        // Block message sending until app is ready (initial conversation created/loaded)
        if !self.is_ready {
            debug!("Not ready yet, ignoring message");
            return;
        }

        let chat_view = self.chat_view.clone();

        // Set streaming state to true (deferred to avoid re-entrancy)
        cx.defer({
            let chat_view = chat_view.clone();
            move |cx| {
                chat_view.update(cx, |view, cx| {
                    view.chat_input_state().update(cx, |input, cx| {
                        input.set_streaming(true, cx);
                    });
                });
            }
        });
        let sidebar = self.sidebar_view.clone();
        let app_entity = cx.entity();
        let repo = self.conversation_repo.clone();

        // Get the conversation ID for task tracking
        // If no conversation exists, we'll create one inside the async block
        let conv_id_for_task = cx.global::<ConversationsStore>().active_id().cloned();
        let needs_conversation_creation = conv_id_for_task.is_none();

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
                                id
                            }
                            Err(e) => {
                                error!(error = ?e, "Failed to create conversation");

                                // Clear streaming state on error (clear all since we don't have a conv_id)
                                app_entity
                                    .update(cx, |app, cx| {
                                        app.clear_streaming_state(None, cx);
                                    })
                                    .map_err(|e| warn!(error = ?e, "Failed to clear streaming state on error"))
                                    .ok();

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

                            Ok((
                                conv.agent().clone(),
                                conv.history().to_vec(),
                                model_id,
                                supports_pdf,
                                supports_images
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

                // Set up global notifier for BashExecutor to use
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
                let (mut stream, user_message) =
                    stream_prompt(&agent, &history, contents, Some(approval_rx), Some(resolution_rx)).await?;

                // Update history synchronously with user message
                cx.update_global::<ConversationsStore, _>(|store, _cx| {
                    if let Some(conv) = store.get_conversation_mut(&conv_id) {
                        conv.add_user_message_with_attachments(user_message, attachments.clone());
                    }
                })
                .map_err(|e| anyhow::anyhow!(e.to_string()))?;

                debug!("Got stream, starting to process");
                use futures::StreamExt;
                let mut response_text = String::new();
                let mut chunk_count = 0;
                let mut token_usage: Option<(u32, u32)> = None;

                // PHASE 4: Process LLM response stream
                debug!("Entering stream processing loop");
                while let Some(chunk_result) = stream.next().await {
                    chunk_count += 1;
                    debug!(chunk_num = chunk_count, chunk = ?chunk_result, "Processing stream chunk");
                    match chunk_result {
                        Ok(StreamChunk::Text(text)) => {
                            debug!(text = %text, "Text chunk received");
                            response_text.push_str(&text);

                            // Update the Conversation model (source of truth)
                            cx.update_global::<ConversationsStore, _>(|store, _cx| {
                                if let Some(conv) = store.get_conversation_mut(&conv_id) {
                                    conv.append_streaming_content(&text);
                                }
                            })
                            .map_err(|e| warn!(error = ?e, "Failed to update conversation streaming content"))
                            .ok();

                            // Update UI if this conversation is currently displayed
                            chat_view
                                .update(cx, |view, cx| {
                                    let view_conv_id = view.conversation_id().cloned();
                                    debug!(view_conv_id = ?view_conv_id, expected_conv_id = %conv_id, "Checking conversation ID");
                                    if view_conv_id.as_ref() == Some(&conv_id) {
                                        debug!("Conversation ID matches, appending text");
                                        view.append_assistant_text(&text, cx);
                                    } else {
                                        warn!("Conversation ID mismatch, text not appended");
                                    }
                                })
                                .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                        }
                        Ok(StreamChunk::ToolCallStarted { id, name }) => {
                            chat_view
                                .update(cx, |view, cx| {
                                    if view.conversation_id() == Some(&conv_id) {
                                        view.handle_tool_call_started(id.clone(), name, cx);
                                    }
                                })
                                .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                        }
                        Ok(StreamChunk::ToolCallInput { id, arguments }) => {
                            chat_view
                                .update(cx, |view, cx| {
                                    if view.conversation_id() == Some(&conv_id) {
                                        view.handle_tool_call_input(id, arguments, cx);
                                    }
                                })
                                .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                        }
                        Ok(StreamChunk::ToolCallResult { id, result }) => {
                            chat_view
                                .update(cx, |view, cx| {
                                    if view.conversation_id() == Some(&conv_id) {
                                        view.handle_tool_call_result(id, result, cx);
                                    }
                                })
                                .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                        }
                        Ok(StreamChunk::ToolCallError { id, error }) => {
                            chat_view
                                .update(cx, |view, cx| {
                                    if view.conversation_id() == Some(&conv_id) {
                                        view.handle_tool_call_error(id, error, cx);
                                    }
                                })
                                .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                        }
                        Ok(StreamChunk::ApprovalRequested { id, command, is_sandboxed }) => {
                            debug!(id = %id, command = %command, sandboxed = is_sandboxed, "Approval requested");

                            // Forward to chat view for UI display
                            chat_view
                                .update(cx, |view, cx| {
                                    if view.conversation_id() == Some(&conv_id) {
                                        view.handle_approval_requested(id, command, is_sandboxed, cx);
                                    }
                                })
                                .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                        }
                        Ok(StreamChunk::ApprovalResolved { id, approved }) => {
                            debug!(id = %id, approved = approved, "Approval resolved");

                            // Update approval state in UI
                            chat_view
                                .update(cx, |view, cx| {
                                    if view.conversation_id() == Some(&conv_id) {
                                        view.handle_approval_resolved(&id, approved, cx);
                                    }
                                })
                                .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                        }
                        Ok(StreamChunk::TokenUsage { input_tokens, output_tokens }) => {
                            debug!(input_tokens = input_tokens, output_tokens = output_tokens, "Received token usage");
                            token_usage = Some((input_tokens, output_tokens));
                        }
                        Ok(StreamChunk::Done) => {
                            debug!("Received Done chunk");
                            // Don't finalize yet - there may still be buffered chunks
                            break;
                        }
                        Ok(StreamChunk::Error(err)) => {
                            error!(error = %err, "Stream error");

                            // Detect authentication errors (401/Unauthorized)
                            if err.contains("401") || err.contains("Unauthorized") {
                                tracing::warn!("Detected Azure auth error - token likely expired");

                                // Attempt to refresh the token for next request
                                if let Some(cache) = cx.update(|cx| {
                                    cx.try_global::<crate::chatty::auth::AzureTokenCache>().cloned()
                                }).ok().flatten() {
                                    if let Err(e) = cache.refresh_token().await {
                                        error!(error = ?e, "Failed to refresh Azure token after 401 error");
                                    } else {
                                        tracing::info!(
                                            "Azure token refreshed successfully. Please retry your message - \
                                            the next request will use a fresh token."
                                        );
                                    }
                                }
                            }

                            break;
                        }
                        Err(e) => {
                            error!(error = %e, "Stream error");
                            break;
                        }
                    }
                }

                // PHASE 5: Finalize response in conversation and UI
                debug!("Stream loop finished, starting finalization");

                // Extract trace before finalizing
                let trace_json = chat_view
                    .update(cx, |view, _cx| view.extract_current_trace())
                    .map_err(|e| anyhow::anyhow!(e.to_string()))?
                    .and_then(|trace| serde_json::to_value(&trace).ok());

                // Finalize UI - stop streaming animation
                debug!("Finalizing UI");
                chat_view
                    .update(cx, |view, cx| {
                        if view.conversation_id() == Some(&conv_id) {
                            view.finalize_assistant_message(cx);
                        }
                    })
                    .map_err(|e| anyhow::anyhow!(e.to_string()))?;

                // Finalize response in conversation
                debug!("Finalizing response in conversation");
                let should_generate_title = cx.update_global::<ConversationsStore, _>(|store, _cx| {
                    if let Some(conv) = store.get_conversation_mut(&conv_id) {
                        conv.finalize_response(response_text.clone());
                        conv.add_trace(trace_json);
                        debug!("Response finalized in conversation");
                        // Check if we should generate a title (first exchange complete)
                        let msg_count = conv.message_count();
                        debug!(msg_count = msg_count, "Message count after finalize");
                        debug!(title = %conv.title(), "Current title");
                        let should_gen = msg_count == 2 && conv.title() == "New Chat";
                        if should_gen {
                            debug!("Will generate title for first exchange");
                        } else if msg_count != 2 {
                            debug!(count = msg_count, "Skipping title generation (count != 2)");
                        } else {
                            debug!("Skipping title generation (title already set)");
                        }
                        should_gen
                    } else {
                        error!("Could not find conversation to finalize");
                        false
                    }
                })
                .map_err(|e| anyhow::anyhow!(e.to_string()))?;

                // PHASE 6: Generate title for first conversation exchange
                if should_generate_title {
                    // Extract agent and history synchronously
                    let title_data = cx
                        .update_global::<ConversationsStore, _>(|store, _cx| {
                            if let Some(conv) = store.get_conversation(&conv_id) {
                                Ok((conv.agent().clone(), conv.history().to_vec()))
                            } else {
                                Err(anyhow::anyhow!("Conversation not found"))
                            }
                        })
                        .map_err(|e| anyhow::anyhow!(e.to_string()))?;

                    if let Ok((agent, history)) = title_data {
                        // Generate title asynchronously (outside update_global)
                        match generate_title(&agent, &history).await {
                            Ok(new_title) => {
                                debug!(title = %new_title, "Generated title");

                                // Update title synchronously
                                cx.update_global::<ConversationsStore, _>(
                                    |store, _cx| {
                                        if let Some(conv) =
                                            store.get_conversation_mut(&conv_id)
                                        {
                                            conv.set_title(new_title.clone());
                                        }
                                    },
                                )
                                .map_err(|e| warn!(error = ?e, "Failed to update conversation title in store"))
                                .ok();

                                // Update sidebar to show new title
                                sidebar
                                    .update(cx, |sidebar, cx| {
                                        let convs = cx
                                            .global::<ConversationsStore>()
                                            .list_all()
                                            .iter()
                                            .map(|c| {
                                                (c.id().to_string(), c.title().to_string(), Some(c.token_usage().total_estimated_cost_usd))
                                            })
                                            .collect::<Vec<_>>();
                                        sidebar.set_conversations(convs, cx);
                                    })
                                    .map_err(|e| warn!(error = ?e, "Failed to update sidebar with new title"))
                                    .ok();
                            }
                            Err(e) => {
                                warn!(error = ?e, "Title generation failed");
                            }
                        }
                    }
                }

                // PHASE 7: Update token usage and save conversation
                if let Some((input_tokens, output_tokens)) = token_usage {
                    debug!(input_tokens = input_tokens, output_tokens = output_tokens, "Processing token usage");

                    // Get model pricing from the conversation's model
                    let model_pricing_result = cx.update_global::<ConversationsStore, _>(|store, _cx| {
                        store.get_conversation(&conv_id).map(|conv| conv.model_id().to_string())
                    });

                    if let Ok(Some(model_id)) = model_pricing_result {
                        let pricing = cx.update_global::<ModelsModel, _>(|models, _cx| {
                            models.get_model(&model_id).and_then(|model| {
                                match (model.cost_per_million_input_tokens, model.cost_per_million_output_tokens) {
                                    (Some(input_cost), Some(output_cost)) => Some((input_cost, output_cost)),
                                    _ => None,
                                }
                            })
                        }).ok().flatten();

                        if let Some((cost_per_million_input, cost_per_million_output)) = pricing {
                            let mut usage = TokenUsage::new(input_tokens, output_tokens);
                            usage.calculate_cost(cost_per_million_input, cost_per_million_output);
                            let cost_usd = usage.estimated_cost_usd;

                            cx.update_global::<ConversationsStore, _>(|store, _cx| {
                                if let Some(conv) = store.get_conversation_mut(&conv_id) {
                                    conv.add_token_usage(usage);
                                }
                            }).map_err(|e| warn!(error = ?e, "Failed to update token usage in store"))
                            .ok();

                            debug!(cost_usd = ?cost_usd, "Token usage updated in conversation");

                            // Update sidebar with refreshed costs
                            debug!("Updating sidebar with new costs");
                            let update_result = sidebar.update(cx, |sidebar, cx| {
                                let convs = cx
                                    .global::<ConversationsStore>()
                                    .list_all()
                                    .iter()
                                    .map(|c| {
                                        let cost = c.token_usage().total_estimated_cost_usd;
                                        debug!(id = %c.id(), cost = ?cost, "Sidebar conversation cost");
                                        (
                                            c.id().to_string(),
                                            c.title().to_string(),
                                            Some(cost),
                                        )
                                    })
                                    .collect::<Vec<_>>();
                                debug!(count = convs.len(), "Setting {} conversations on sidebar", convs.len());
                                sidebar.set_conversations(convs, cx);
                            });
                            if let Err(e) = update_result {
                                warn!(error = ?e, "Failed to update sidebar with costs");
                            } else {
                                debug!("Sidebar updated successfully with new costs");
                            }
                        } else {
                            debug!("No pricing information available for model");
                        }
                    }
                }

                // Persist to disk
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
                }

                // Clear streaming state on success for this conversation
                let conv_id_for_clear = conv_id.clone();
                app_entity
                    .update(cx, |app, cx| {
                        app.clear_streaming_state(Some(&conv_id_for_clear), cx);
                        // Also remove placeholder if it still exists
                        app.active_stream_tasks.remove("__pending__");
                    })
                    .map_err(|e| warn!(error = ?e, "Failed to clear streaming state"))
                    .ok();

                Ok(())
            });

        // Store the task in HashMap with conversation ID as key
        // This allows multiple conversations to have concurrent streams
        if let Some(conv_id) = conv_id_for_task {
            self.active_stream_tasks.insert(conv_id, task);
        } else if needs_conversation_creation {
            // No conversation exists yet - use placeholder key
            // The async block will move this to the real conversation ID once created
            self.active_stream_tasks
                .insert("__pending__".to_string(), task);
            debug!("Using placeholder key for task until conversation is created");
        }
    }

    /// Stop the currently active stream for the current conversation
    pub fn stop_stream(&mut self, cx: &mut Context<Self>) {
        // Get current conversation ID
        let conv_id_opt = cx
            .try_global::<ConversationsStore>()
            .and_then(|store| store.active_id().cloned());

        if let Some(conv_id) = &conv_id_opt {
            // Remove and drop the stream task for this specific conversation
            if let Some(task) = self.active_stream_tasks.remove(conv_id) {
                debug!(conversation_id = %conv_id, "Cancelling stream for conversation");
                // Simply drop the task - GPUI will cancel it automatically
                drop(task);
            } else {
                // Try the pending key (in case stream started before conversation was created)
                if let Some(task) = self.active_stream_tasks.remove("__pending__") {
                    debug!(conversation_id = %conv_id, "Cancelling pending stream that became this conversation");
                    drop(task);
                } else {
                    debug!(conversation_id = %conv_id, "No active stream found for conversation");
                    // No active stream, might be a race condition - still reset UI
                }
            }
        } else {
            // No conversation ID yet - check for pending stream
            if let Some(task) = self.active_stream_tasks.remove("__pending__") {
                debug!("Cancelling pending stream (no conversation yet)");
                drop(task);
            }
        }

        let chat_view = self.chat_view.clone();
        let repo = self.conversation_repo.clone();

        if let Some(conv_id) = conv_id_opt {
            // Extract trace before finalizing UI
            let trace_json = chat_view.update(cx, |view, _cx| {
                view.extract_current_trace()
                    .and_then(|trace| serde_json::to_value(&trace).ok())
            });

            // Mark the assistant message as cancelled in UI
            chat_view.update(cx, |view, cx| {
                view.mark_message_cancelled(cx);
            });

            // Get the partial response text from the UI (after marking as cancelled)
            let response_text = chat_view
                .read(cx)
                .messages()
                .last()
                .map(|msg| msg.content.clone())
                .unwrap_or_default();

            // Save the partial response to conversation history (includes cancellation notice)
            cx.update_global::<ConversationsStore, _>(|store, _cx| {
                if let Some(conv) = store.get_conversation_mut(&conv_id) {
                    conv.finalize_response(response_text);
                    conv.add_trace(trace_json);
                    debug!("Partial response saved to conversation after stop");
                }
            });

            // Serialize conversation data for saving
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
                        created_at: conv
                            .created_at()
                            .duration_since(SystemTime::UNIX_EPOCH)
                            .unwrap()
                            .as_secs() as i64,
                        updated_at: now,
                    })
                })
            });

            // Save to disk asynchronously
            if let Some(conv_data) = conv_data_opt {
                let conv_id_for_save = conv_id.clone();
                cx.spawn(async move |_, _cx| {
                    if let Err(e) = repo.save(&conv_id_for_save, conv_data).await {
                        warn!(error = ?e, "Failed to save conversation after stop");
                    } else {
                        debug!("Conversation saved to disk after stop");
                    }
                    Ok::<_, anyhow::Error>(())
                })
                .detach();
            }
        }

        cx.notify();
    }

    /// Clear streaming state for a specific conversation or all conversations
    ///
    /// If conversation_id is provided, clears only that conversation's stream.
    /// If None, clears all streams (useful for shutdown scenarios).
    ///
    /// Only updates UI if the cleared conversation is currently active.
    fn clear_streaming_state(&mut self, conversation_id: Option<&str>, cx: &mut Context<Self>) {
        let should_update_ui = if let Some(conv_id) = conversation_id {
            // Clear specific conversation's stream
            self.active_stream_tasks.remove(conv_id);

            // Clear streaming message from Conversation model
            cx.update_global::<ConversationsStore, _>(|store, _cx| {
                if let Some(conv) = store.get_conversation_mut(conv_id) {
                    conv.set_streaming_message(None);
                }
            });

            // Check if this is the currently active conversation
            let current_conv = self
                .chat_view
                .read(cx)
                .conversation_id()
                .map(|s| s.as_str());
            current_conv == Some(conv_id)
        } else {
            // Clear all streams
            self.active_stream_tasks.clear();
            // Note: Not clearing streaming messages from all conversations here
            // as they will be cleared individually when streams complete
            // Always update UI when clearing all
            true
        };

        // Only update UI if the cleared conversation is currently displayed
        if should_update_ui {
            self.chat_view.update(cx, |view, cx| {
                view.chat_input_state().update(cx, |input, cx| {
                    input.set_streaming(false, cx);
                });
            });
        }
    }

    /// Check if any stream is currently active
    #[allow(dead_code)]
    pub fn is_streaming(&self) -> bool {
        !self.active_stream_tasks.is_empty()
    }

    /// Check if a specific conversation has an active stream
    #[allow(dead_code)]
    pub fn is_conversation_streaming(&self, conversation_id: &str) -> bool {
        self.active_stream_tasks.contains_key(conversation_id)
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
