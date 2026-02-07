use gpui::*;
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

        // Auto-create first conversation if none exist
        let has_convs = cx
            .try_global::<ConversationsStore>()
            .map(|store| store.count() > 0)
            .unwrap_or(false);

        if !has_convs {
            info!("No conversations, creating initial one");

            // Check if models are available
            let has_models = cx
                .try_global::<ModelsModel>()
                .map(|m| !m.models().is_empty())
                .unwrap_or(false);

            if has_models {
                // Models already loaded, create immediately and wait for completion
                info!("Models available, creating now");
                let app_entity = cx.entity();
                cx.spawn(async move |_, cx| {
                    let task_result: Result<gpui::Task<anyhow::Result<String>>, _> =
                        app_entity.update(cx, |app, cx| app.create_new_conversation(cx));
                    if let Ok(task) = task_result {
                        let _ = task.await;
                    }
                    let _: Result<(), _> = app_entity.update(cx, |app, cx| {
                        app.is_ready = true;
                        info!("App is now ready (initial conversation created)");
                        cx.notify();
                    });
                    Ok::<_, anyhow::Error>(())
                })
                .detach();
            } else {
                // Models not loaded yet, defer until after first render
                info!("Models not ready, will defer creation");
                let app_entity = cx.entity();
                cx.defer(move |cx| {
                    app_entity.update(cx, |_app, cx| {
                        let has_models = cx
                            .try_global::<ModelsModel>()
                            .map(|m| !m.models().is_empty())
                            .unwrap_or(false);

                        if has_models {
                            info!("Models now available, creating conversation");
                            let app_entity_inner = cx.entity();
                            cx.spawn(async move |_, cx| {
                                let task_result: Result<gpui::Task<anyhow::Result<String>>, _> =
                                    app_entity_inner
                                        .update(cx, |app, cx| app.create_new_conversation(cx));
                                if let Ok(task) = task_result {
                                    let _ = task.await;
                                }
                                let _: Result<(), _> = app_entity_inner.update(cx, |app, cx| {
                                    app.is_ready = true;
                                    info!("App is now ready (deferred conversation created)");
                                    cx.notify();
                                });
                                Ok::<_, anyhow::Error>(())
                            })
                            .detach();
                        } else {
                            warn!("Models still not available");
                        }
                    });
                });
            }
        }

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

        // New chat callback
        sidebar.update(cx, |sidebar, _cx| {
            let app = app_entity.clone();
            sidebar.set_on_new_chat(move |cx| {
                let app = app.clone();
                app.update(cx, |app, cx| {
                    app.create_new_conversation(cx).detach();
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
    async fn restore_conversation_from_data(
        data: ConversationData,
        models: &ModelsModel,
        providers: &ProviderModel,
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

        // Restore conversation using factory method
        Conversation::from_data(data, model_config, provider_config).await
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

                    match (models_result, providers_result) {
                        (Ok(models), Ok(providers)) => {
                            let mut restored_count = 0;
                            let mut failed_count = 0;

                            // Reconstruct each conversation
                            for data in conversation_data {
                                let conv_id = data.id.clone();

                                match Self::restore_conversation_from_data(
                                    data, &models, &providers,
                                )
                                .await
                                {
                                    Ok(conversation) => {
                                        // Add to global store
                                        cx.update_global::<ConversationsStore, _>(|store, _cx| {
                                            store.add_conversation(conversation);
                                        })
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
                            }).ok();

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
                                .ok();

                            // Don't set any conversation as active in the store or chat view
                            // This ensures when the user sends the first message, a new conversation is created
                            chat_view
                                .update(cx, |view, cx| {
                                    view.set_conversation_id(String::new());
                                    view.clear_messages(cx);
                                    cx.notify();
                                })
                                .ok();

                            // Mark app as ready
                            if let Some(app) = weak.upgrade() {
                                let _: Result<(), _> = app.update(cx, |app, cx| {
                                    app.is_ready = true;
                                    info!("App is now ready (conversations loaded)");
                                    cx.notify();
                                });
                            }
                        }
                        _ => {
                            error!("Failed to access global stores");
                        }
                    }
                }
                Err(e) => {
                    error!(error = ?e, "Failed to load conversation files");
                }
            }

            Ok::<_, anyhow::Error>(())
        })
        .detach();
    }

    /// Create a new conversation
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

                cx.spawn(async move |_weak, cx| {
                    // Create conversation
                    let conv_id = uuid::Uuid::new_v4().to_string();
                    let title = "New Chat".to_string();

                    let conversation = Conversation::new(
                        conv_id.clone(),
                        title.clone(),
                        &model_config,
                        &provider_config,
                    )
                    .await?;

                    // Add to global store
                    cx.update_global::<ConversationsStore, _>(|store, _cx| {
                        store.add_conversation(conversation);
                        store.set_active(conv_id.clone());
                    })?;

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
                        debug!(
                            conv_count = convs.len(),
                            "Updating sidebar with conversations"
                        );
                        sidebar.set_conversations(convs, cx);
                        sidebar.set_active_conversation(Some(conv_id.clone()), cx);
                        debug!("Sidebar updated with new conversation");
                    })?;

                    // Update chat view
                    chat_view.update(cx, |view, cx| {
                        view.set_conversation_id(conv_id.clone());
                        view.clear_messages(cx);

                        // Set available models in chat input
                        let models_list: Vec<(String, String)> = cx
                            .global::<ModelsModel>()
                            .models()
                            .iter()
                            .map(|m| (m.id.clone(), m.name.clone()))
                            .collect();

                        view.chat_input_state().update(cx, |state, _cx| {
                            state.set_available_models(models_list, Some(model_config.id.clone()));
                            state.set_capabilities(model_config.supports_images, model_config.supports_pdf);
                        });
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
        // Set active in store
        cx.update_global::<ConversationsStore, _>(|store, _cx| {
            store.set_active(id.to_string());
        });

        let conv_id = id.to_string();
        let chat_view = self.chat_view.clone();
        let sidebar = self.sidebar_view.clone();

        // Update sidebar active state
        sidebar.update(cx, |sidebar, cx| {
            sidebar.set_active_conversation(Some(conv_id.clone()), cx);
        });

        // Update chat view
        let conversation_data =
            cx.global::<ConversationsStore>()
                .get_conversation(id)
                .map(|conv| {
                    (
                        conv.history().to_vec(),
                        conv.system_traces().to_vec(),
                        conv.attachment_paths().to_vec(),
                        conv.model_id().to_string(),
                    )
                });

        if let Some((history, traces, attachment_paths, model_id)) = conversation_data {
            chat_view.update(cx, |view, cx| {
                view.set_conversation_id(conv_id.clone());
                view.load_history(&history, &traces, &attachment_paths, cx);

                // Update the selected model and capabilities in the chat input
                let model_capabilities = cx
                    .global::<ModelsModel>()
                    .get_model(&model_id)
                    .map(|m| (m.supports_images, m.supports_pdf))
                    .unwrap_or((false, false));

                view.chat_input_state().update(cx, |state, _cx| {
                    state.set_selected_model_id(model_id);
                    state.set_capabilities(model_capabilities.0, model_capabilities.1);
                });
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
                        // Create new agent asynchronously (outside update_global to avoid blocking)
                        let new_agent =
                            AgentClient::from_model_config(&model_config, &provider_config).await?;

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
                view.set_conversation_id(String::new());
            });
        }

        // Delete from disk
        cx.spawn(async move |_weak, _cx| {
            repo.delete(&conv_id).await.ok();
            Ok::<_, anyhow::Error>(())
        })
        .detach();
    }

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
        let repo = self.conversation_repo.clone();

        // Get active conversation and send message
        debug!("Spawning async task for LLM call");
        cx.spawn(async move |_weak, cx| -> anyhow::Result<()> {
                debug!("Async task started");

                // Get active conversation ID, or create a new one if it doesn't exist
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
                        let task = app_entity.update(cx, |app, cx| app.create_new_conversation(cx))
                            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                        match task.await {
                            Ok(id) => {
                                debug!(conv_id = %id, "Created new conversation");
                                id
                            }
                            Err(e) => {
                                error!(error = ?e, "Failed to create conversation");
                                return Err(e);
                            }
                        }
                    }
                };

                // Now we have a conversation ID for sure, set it on the chat view
                // and add the user/assistant messages AFTER conversation exists
                chat_view.update(cx, |view, cx| {
                    view.set_conversation_id(conv_id.clone());
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
                }).ok();

                // Extract agent and history synchronously (to avoid blocking in async context)
                let (agent, history) = cx
                    .update_global::<ConversationsStore, _>(|store, _cx| {
                        if let Some(conv) = store.get_conversation(&conv_id) {
                            Ok((conv.agent().clone(), conv.history().to_vec()))
                        } else {
                            Err(anyhow::anyhow!(
                                "Could not find conversation after creation/lookup"
                            ))
                        }
                    })
                    .map_err(|e| anyhow::anyhow!(e.to_string()))??;

                // Create stream asynchronously (outside update_global to avoid blocking)
                debug!("Calling stream_prompt()");
                let mut contents = vec![rig::message::UserContent::Text(
                    rig::completion::message::Text {
                        text: message.clone(),
                    },
                )];

                // Convert file attachments to UserContent
                // Filter based on provider capabilities to prevent panics in rig-core
                let provider_supports_pdf = matches!(
                    &agent,
                    crate::chatty::factories::AgentClient::Anthropic(_)
                        | crate::chatty::factories::AgentClient::Gemini(_)
                );
                let provider_supports_images = !matches!(
                    &agent,
                    crate::chatty::factories::AgentClient::Mistral(_)
                );
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
                    match attachment_to_user_content(path) {
                        Ok(content) => contents.push(content),
                        Err(e) => warn!(?path, error = ?e, "Failed to convert attachment"),
                    }
                }
                let (mut stream, user_message) =
                    stream_prompt(&agent, &history, contents).await?;

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

                // Process stream
                debug!("Entering stream processing loop");
                while let Some(chunk_result) = stream.next().await {
                    chunk_count += 1;
                    debug!(chunk_num = chunk_count, chunk = ?chunk_result, "Processing stream chunk");
                    match chunk_result {
                        Ok(StreamChunk::Text(text)) => {
                            debug!(text = %text, "Text chunk received");
                            response_text.push_str(&text);

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
                                        view.handle_tool_call_started(id, name, cx);
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
                            break;
                        }
                        Err(e) => {
                            error!(error = %e, "Stream error");
                            break;
                        }
                    }
                }

                // Stream loop finished - now perform all finalization
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

                // Generate title if this was the first exchange
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
                                    .ok();
                            }
                            Err(e) => {
                                warn!(error = ?e, "Title generation failed");
                            }
                        }
                    }
                }

                // Update token usage if available
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
                            }).ok();

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

                Ok(())
            })
            .detach();
    }
}

/// Convert a file attachment to a rig-core UserContent
fn attachment_to_user_content(path: &Path) -> anyhow::Result<rig::message::UserContent> {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();

    let data = std::fs::read(path)?;
    let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &data);

    match ext.as_str() {
        "png" => Ok(rig::message::UserContent::image_base64(
            b64,
            Some(rig::completion::message::ImageMediaType::PNG),
            None,
        )),
        "jpg" | "jpeg" => Ok(rig::message::UserContent::image_base64(
            b64,
            Some(rig::completion::message::ImageMediaType::JPEG),
            None,
        )),
        "gif" => Ok(rig::message::UserContent::image_base64(
            b64,
            Some(rig::completion::message::ImageMediaType::GIF),
            None,
        )),
        "webp" => Ok(rig::message::UserContent::image_base64(
            b64,
            Some(rig::completion::message::ImageMediaType::WEBP),
            None,
        )),
        "svg" => Ok(rig::message::UserContent::image_base64(
            b64,
            Some(rig::completion::message::ImageMediaType::SVG),
            None,
        )),
        "pdf" => Ok(rig::message::UserContent::document(
            b64,
            Some(rig::completion::message::DocumentMediaType::PDF),
        )),
        _ => Err(anyhow::anyhow!("Unsupported file type: {}", ext)),
    }
}
