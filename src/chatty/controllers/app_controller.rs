use gpui::*;
use std::sync::Arc;
use std::time::SystemTime;

use crate::chatty::models::{Conversation, ConversationsModel, StreamChunk};
use crate::chatty::repositories::{
    ConversationData, ConversationJsonRepository, ConversationRepository,
};
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
}

impl ChattyApp {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Initialize global conversations model if not already done
        if !cx.has_global::<ConversationsModel>() {
            cx.set_global(ConversationsModel::new());
        }

        // Create repository
        let conversation_repo: Arc<dyn ConversationRepository> = Arc::new(
            ConversationJsonRepository::new().expect("Failed to create conversation repository"),
        );

        // Create views
        let chat_view = cx.new(|cx| ChatView::new(window, cx));
        let sidebar_view = cx.new(|_cx| SidebarView::new());

        let mut app = Self {
            chat_view,
            sidebar_view,
            conversation_repo,
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
            .try_global::<ConversationsModel>()
            .map(|store| store.count() > 0)
            .unwrap_or(false);

        if !has_convs {
            eprintln!("🆕 [ChattyApp::new] No conversations, creating initial one");

            // Check if models are available
            let has_models = cx
                .try_global::<ModelsModel>()
                .map(|m| !m.models().is_empty())
                .unwrap_or(false);

            if has_models {
                // Models already loaded, create immediately
                eprintln!("🆕 [ChattyApp::new] Models available, creating now");
                app.create_new_conversation(cx).detach();
            } else {
                // Models not loaded yet, defer until after first render
                eprintln!("🆕 [ChattyApp::new] Models not ready, will defer creation");
                let app_entity = cx.entity();
                cx.defer(move |cx| {
                    app_entity.update(cx, |app, cx| {
                        let has_models = cx
                            .try_global::<ModelsModel>()
                            .map(|m| !m.models().is_empty())
                            .unwrap_or(false);

                        if has_models {
                            eprintln!("🆕 [ChattyApp::new deferred] Models now available, creating conversation");
                            app.create_new_conversation(cx).detach();
                        } else {
                            eprintln!(
                                "⚠️  [ChattyApp::new deferred] Models still not available"
                            );
                        }
                    });
                });
            }
        }

        app
    }

    /// Get chat view entity
    pub fn chat_view(&self) -> &Entity<ChatView> {
        &self.chat_view
    }

    /// Get sidebar view entity
    pub fn sidebar_view(&self) -> &Entity<SidebarView> {
        &self.sidebar_view
    }

    /// Load conversations after models and providers are ready
    /// This should be called from main.rs after both models and providers have been loaded
    pub fn load_conversations_after_models_ready(&self, cx: &mut Context<Self>) {
        eprintln!(
            "📂 [ChattyApp::load_conversations_after_models_ready] Starting conversation load"
        );
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

        // Chat input send message callback
        chat_view.update(cx, |view, cx| {
            let input_state = view.chat_input_state().clone();
            let app_for_send = app_entity.clone();
            input_state.update(cx, |state, _cx| {
                eprintln!("🔧 [AppController] Setting up on_send callback for chat input");
                state.set_on_send(move |message, cx| {
                    eprintln!(
                        "📨 [AppController] on_send callback triggered with: '{}'",
                        message
                    );
                    let app = app_for_send.clone();
                    let msg = message.clone();

                    // Update directly without defer
                    eprintln!("✅ [AppController] Calling send_message directly via entity");
                    app.update(cx, |app, cx| {
                        app.send_message(msg, cx);
                    });
                });
            });
        });

        // Chat input model change callback
        chat_view.update(cx, |view, cx| {
            let input_state = view.chat_input_state().clone();
            let app_for_model = app_entity.clone();
            input_state.update(cx, |state, _cx| {
                eprintln!("🔧 [AppController] Setting up on_model_change callback for chat input");
                state.set_on_model_change(move |model_id, cx| {
                    eprintln!(
                        "🔄 [AppController] on_model_change callback triggered with model: '{}'",
                        model_id
                    );
                    let app = app_for_model.clone();
                    let model_id = model_id.clone();

                    app.update(cx, |app, cx| {
                        app.change_conversation_model(model_id, cx);
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

            // Set available models on chat input
            chat_view.update(cx, |view, cx| {
                view.chat_input_state().update(cx, |state, _cx| {
                    state.set_available_models(models_list, default_model_id);
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

        cx.spawn(async move |_weak, mut cx| {
            match repo.load_all().await {
                Ok(conversation_data) => {
                    eprintln!(
                        "📂 [load_conversations] Loaded {} conversation files",
                        conversation_data.len()
                    );

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
                                        cx.update_global::<ConversationsModel, _>(|store, _cx| {
                                            store.add_conversation(conversation);
                                        })
                                        .ok();

                                        restored_count += 1;
                                        eprintln!("✅ [load_conversations] Restored: {}", conv_id);
                                    }
                                    Err(e) => {
                                        failed_count += 1;
                                        eprintln!(
                                            "⚠️  [load_conversations] Failed to restore {}: {:?}",
                                            conv_id, e
                                        );
                                    }
                                }
                            }

                            eprintln!(
                                "📊 [load_conversations] Restored: {}, Failed: {}",
                                restored_count, failed_count
                            );

                            // Update sidebar with all conversations
                            sidebar
                                .update(cx, |sidebar, cx| {
                                    let convs = cx
                                        .global::<ConversationsModel>()
                                        .list_all()
                                        .iter()
                                        .map(|c| (c.id().to_string(), c.title().to_string()))
                                        .collect::<Vec<_>>();
                                    sidebar.set_conversations(convs, cx);

                                    // Set active conversation to the most recently updated
                                    if let Some(active_conv) =
                                        cx.global::<ConversationsModel>().list_all().first()
                                    {
                                        let active_id = active_conv.id().to_string();
                                        cx.update_global::<ConversationsModel, _>(|store, _| {
                                            store.set_active(active_id.clone());
                                        });
                                        sidebar.set_active_conversation(Some(active_id), cx);
                                    }
                                })
                                .ok();
                        }
                        _ => {
                            eprintln!("❌ [load_conversations] Failed to access global stores");
                        }
                    }
                }
                Err(e) => {
                    eprintln!(
                        "❌ [load_conversations] Failed to load conversation files: {:?}",
                        e
                    );
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
        eprintln!("🆕 [AppController::create_new_conversation] Creating new conversation");
        // Get first available model
        let models = cx.global::<ModelsModel>();
        let providers = cx.global::<ProviderModel>();

        if let Some(model_config) = models.models().first() {
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

                cx.spawn(async move |_weak, mut cx| {
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
                    cx.update_global::<ConversationsModel, _>(|store, _cx| {
                        store.add_conversation(conversation);
                        store.set_active(conv_id.clone());
                    })?;

                    // Update sidebar
                    sidebar.update(cx, |sidebar, cx| {
                        let convs = cx
                            .global::<ConversationsModel>()
                            .list_all()
                            .iter()
                            .map(|c| (c.id().to_string(), c.title().to_string()))
                            .collect::<Vec<_>>();
                        sidebar.set_conversations(convs, cx);
                        sidebar.set_active_conversation(Some(conv_id.clone()), cx);
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
                eprintln!("{}", err_msg);
                Task::ready(Err(anyhow::anyhow!(err_msg)))
            }
        } else {
            let err_msg = "No models configured";
            eprintln!("{}", err_msg);
            // TODO: Show error in UI
            Task::ready(Err(anyhow::anyhow!(err_msg)))
        }
    }

    /// Load a conversation by ID
    fn load_conversation(&mut self, id: &str, cx: &mut Context<Self>) {
        // Set active in store
        cx.update_global::<ConversationsModel, _>(|store, _cx| {
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
            cx.global::<ConversationsModel>()
                .get_conversation(id)
                .map(|conv| {
                    (
                        conv.history().to_vec(),
                        conv.system_traces().to_vec(),
                        conv.model_id().to_string(),
                    )
                });

        if let Some((history, traces, model_id)) = conversation_data {
            chat_view.update(cx, |view, cx| {
                view.set_conversation_id(conv_id.clone());
                view.load_history(&history, &traces, cx);

                // Update the selected model in the chat input
                view.chat_input_state().update(cx, |state, _cx| {
                    state.set_selected_model_id(model_id);
                });
            });
        }
    }

    /// Change the model for the active conversation
    fn change_conversation_model(&mut self, model_id: String, cx: &mut Context<Self>) {
        eprintln!(
            "🔄 [AppController::change_conversation_model] Changing to model: '{}'",
            model_id
        );

        // Get the active conversation ID
        let conv_id = cx
            .global::<ConversationsModel>()
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

                    eprintln!(
                        "✅ [AppController::change_conversation_model] Found model and provider config"
                    );

                    // Update the conversation model
                    cx.spawn(async move |_weak, mut cx| -> anyhow::Result<()> {
                        // Update the conversation's model and agent
                        cx.update_global::<ConversationsModel, _>(|store, _cx| {
                            if let Some(conv) = store.get_conversation_mut(&conv_id) {
                                eprintln!("🔄 [AppController async] Updating conversation model");
                                smol::block_on(conv.update_model(&model_config, &provider_config))
                            } else {
                                Err(anyhow::anyhow!("Conversation not found"))
                            }
                        })
                        .map_err(|e| anyhow::anyhow!(e.to_string()))??;

                        eprintln!("✅ [AppController async] Model updated successfully");

                        // Save to disk
                        let conv_data_res =
                            cx.update_global::<ConversationsModel, _>(|store, _cx| {
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
                            eprintln!("💾 [AppController async] Conversation saved to disk");
                        }

                        Ok(())
                    })
                    .detach();
                } else {
                    eprintln!("❌ [AppController::change_conversation_model] Provider not found");
                }
            } else {
                eprintln!("❌ [AppController::change_conversation_model] Model not found");
            }
        } else {
            eprintln!("❌ [AppController::change_conversation_model] No active conversation");
        }
    }

    /// Delete a conversation
    fn delete_conversation(&mut self, id: &str, cx: &mut Context<Self>) {
        let conv_id = id.to_string();
        let repo = self.conversation_repo.clone();
        let sidebar = self.sidebar_view.clone();
        let chat_view = self.chat_view.clone();

        // Remove from global store
        cx.update_global::<ConversationsModel, _>(|store, _cx| {
            store.delete_conversation(&conv_id);
        });

        // Update sidebar
        sidebar.update(cx, |sidebar, cx| {
            let convs = cx
                .global::<ConversationsModel>()
                .list_all()
                .iter()
                .map(|c| (c.id().to_string(), c.title().to_string()))
                .collect::<Vec<_>>();

            let active_id = cx
                .global::<ConversationsModel>()
                .active_id()
                .map(|s| s.to_string());

            sidebar.set_conversations(convs, cx);
            sidebar.set_active_conversation(active_id.clone(), cx);
        });

        // If deleted conversation was active, clear chat view or load new active
        let active_id = cx
            .global::<ConversationsModel>()
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

    fn send_message(&mut self, message: String, cx: &mut Context<Self>) {
        eprintln!(
            "🚀 [AppController::send_message] Called with message: '{}'",
            message
        );

        let chat_view = self.chat_view.clone();
        let sidebar = self.sidebar_view.clone();

        // Add user message to UI immediately so it appears responsive
        chat_view.update(cx, |view, cx| {
            view.add_user_message(message.clone(), cx);
        });
        eprintln!("✅ [AppController::send_message] User message added to UI");

        // Start assistant message in UI
        chat_view.update(cx, |view, cx| {
            view.start_assistant_message(cx);
        });
        eprintln!("✅ [AppController::send_message] Assistant message started");

        let app_entity = cx.entity();
        let repo = self.conversation_repo.clone();

        // Get active conversation and send message
        eprintln!("🔄 [AppController::send_message] Spawning async task for LLM call");
        cx.spawn(async move |_weak, mut cx| -> anyhow::Result<()> {
                eprintln!("⚡ [AppController::send_message async] Async task started");

                // Get active conversation ID, or create a new one if it doesn't exist
                let conv_id: String = match cx
                    .update_global::<ConversationsModel, _>(|store, _| store.active_id().cloned())
                    .map_err(|e| anyhow::anyhow!(e.to_string()))?
                {
                    Some(id) => {
                        eprintln!("✅ [AppController async] Found active conversation: {}", &id);
                        id
                    }
                    None => {
                        eprintln!("❌ [AppController async] No active conversation found, creating one.");
                        let task = app_entity.update(cx, |app, cx| app.create_new_conversation(cx))
                            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                        match task.await {
                            Ok(id) => {
                                eprintln!("✅ [AppController async] Created new conversation: {}", &id);
                                id
                            }
                            Err(e) => {
                                eprintln!("❌ [AppController async] Failed to create conversation: {:?}", e);
                                return Err(e);
                            }
                        }
                    }
                };

                // Now we have a conversation ID for sure, set it on the chat view
                chat_view.update(cx, |view, cx| {
                    view.set_conversation_id(conv_id.clone());
                    cx.notify();
                }).map_err(|e| anyhow::anyhow!(e.to_string()))?;
                eprintln!("✅ [AppController async] Set conversation ID on chat view: {}", &conv_id);

                // Get the stream
                let mut stream = cx.update_global::<ConversationsModel, _>(|store, _cx| {
                    if let Some(conv) = store.get_conversation_mut(&conv_id) {
                        eprintln!("📤 [AppController async] Calling conv.send_text()");
                        smol::block_on(conv.send_text(message))
                    } else {
                        Err(anyhow::anyhow!("Could not find conversation after creation/lookup"))
                    }
                }).map_err(|e| anyhow::anyhow!(e.to_string()))??;

                eprintln!("✅ [AppController async] Got stream, starting to process");
                use futures::StreamExt;
                let mut response_text = String::new();
                let mut chunk_count = 0;

                // Process stream
                eprintln!("🔄 [AppController async] Entering stream processing loop");
                while let Some(chunk_result) = stream.next().await {
                    chunk_count += 1;
                    eprintln!(
                        "📦 [AppController async] Chunk #{}: {:?}",
                        chunk_count, chunk_result
                    );
                    match chunk_result {
                        Ok(StreamChunk::Text(text)) => {
                            eprintln!("📝 [AppController async] Text chunk: '{}'", text);
                            response_text.push_str(&text);

                            chat_view
                                .update(cx, |view, cx| {
                                    if view.conversation_id() == Some(&conv_id) {
                                        view.append_assistant_text(&text, cx);
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
                        Ok(StreamChunk::Done) => {
                            eprintln!("🏁 [AppController async] Received Done chunk, finalizing");
                            // Extract trace before finalizing
                                                    let trace_json = chat_view
                                                        .update(cx, |view, _cx| view.extract_current_trace())
                                                        .map_err(|e| anyhow::anyhow!(e.to_string()))?
                                                        .and_then(|trace| serde_json::to_value(&trace).ok());
                            // Finalize response in conversation
                            eprintln!("💾 [AppController async] Finalizing response in conversation");
                            let should_generate_title = cx.update_global::<ConversationsModel, _>(|store, _cx| {
                                if let Some(conv) = store.get_conversation_mut(&conv_id) {
                                    conv.finalize_response(response_text.clone());
                                    conv.add_trace(trace_json);
                                    eprintln!("✅ [AppController async] Response finalized in conversation");
                                    // Check if we should generate a title (first exchange complete)
                                    let msg_count = conv.message_count();
                                    eprintln!("📊 [AppController async] Message count after finalize: {}", msg_count);
                                    eprintln!("�� [AppController async] Current title: '{}'", conv.title());
                                    let should_gen = msg_count == 2 && conv.title() == "New Chat";
                                    if should_gen {
                                        eprintln!("🏷️  [AppController async] Will generate title for first exchange");
                                    } else if msg_count != 2 {
                                        eprintln!("⏭️  [AppController async] Skipping title generation (count = {} != 2)", msg_count);
                                    } else {
                                        eprintln!("⏭️  [AppController async] Skipping title generation (title already set)");
                                    }
                                    should_gen
                                } else {
                                    eprintln!("❌ [AppController async] Could not find conversation to finalize");
                                    false
                                }
                            })
                            .map_err(|e| anyhow::anyhow!(e.to_string()))?;

                            // Generate title if this was the first exchange
                            if should_generate_title {
                                let title_result = cx
                                    .update_global::<ConversationsModel, _>(|store, _cx| {
                                        if let Some(conv) = store.get_conversation_mut(&conv_id) {
                                            smol::block_on(conv.generate_and_set_title())
                                        } else {
                                            Err(anyhow::anyhow!("Conversation not found"))
                                        }
                                    })
                                    .map_err(|e| anyhow::anyhow!(e.to_string()))?;

                                match title_result {
                                    Ok(new_title) => {
                                        eprintln!("✅ [AppController async] Generated title: '{}'", new_title);

                                        // Update sidebar to show new title
                                        sidebar
                                            .update(cx, |sidebar, cx| {
                                                let convs = cx
                                                    .global::<ConversationsModel>()
                                                    .list_all()
                                                    .iter()
                                                    .map(|c| (c.id().to_string(), c.title().to_string()))
                                                    .collect::<Vec<_>>();
                                                sidebar.set_conversations(convs, cx);
                                            })
                                            .ok();
                                    }
                                    Err(e) => {
                                        eprintln!("⚠️  [AppController async] Title generation failed: {:?}", e);
                                    }
                                }
                            }

                            // Finalize UI
                            eprintln!("🎨 [AppController async] Finalizing UI");
                            chat_view
                                .update(cx, |view, cx| {
                                    if view.conversation_id() == Some(&conv_id) {
                                        view.finalize_assistant_message(cx);
                                    }
                                })
                                .map_err(|e| anyhow::anyhow!(e.to_string()))?;

                            // Persist to disk
                            let conv_data_res =
                                cx.update_global::<ConversationsModel, _>(|store, _cx| {
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
                            break;
                        }
                        Ok(StreamChunk::Error(err)) => {
                            eprintln!("Stream error: {}", err);
                            break;
                        }
                        Err(e) => {
                            eprintln!("Stream error: {}", e);
                            break;
                        }
                    }
                }

                Ok(())
            })
            .detach();
    }
}
