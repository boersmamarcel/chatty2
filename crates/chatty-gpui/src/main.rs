// Hide console window on Windows in release builds
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use gpui::*;
use gpui_component::*;
use tracing::{debug, error, info, warn};

mod assets;
mod auto_updater;
mod chatty;
mod cli_installer;
mod settings;

use assets::ChattyAssets;
use auto_updater::AutoUpdater;
use chatty::repositories::{ConversationRepository, ConversationSqliteRepository};
use chatty::{ChattyApp, GlobalChattyApp};
use settings::SettingsView;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Upgrade the global weak ChattyApp entity and run a closure on it.
/// No-op if the entity hasn't been registered or has been dropped.
fn with_chatty_app(cx: &mut App, f: impl FnOnce(&mut ChattyApp, &mut Context<ChattyApp>)) {
    if let Some(weak) = cx
        .try_global::<GlobalChattyApp>()
        .and_then(|g| g.entity.clone())
        && let Some(entity) = weak.upgrade()
    {
        entity.update(cx, f);
    }
}

/// Global signal that fires once the agent memory service has finished initializing
/// (successfully or not). Conversation creation awaits this to avoid a race where the
/// agent would be built without memory tools.
///
/// Uses a `watch` channel so that late subscribers (who arrive after init completes)
/// can still observe the `true` state without missing the notification.
pub struct MemoryInitSignal(pub tokio::sync::watch::Receiver<bool>);
impl Global for MemoryInitSignal {}

// Use global singletons from chatty-core
use chatty_core::{MCP_SERVICE, MCP_UPDATE_SENDER};

/// Flag to prevent theme observer from saving during initialization.
/// This avoids a race condition where the default theme could overwrite
/// the user's saved theme preference before settings are loaded.
static THEME_INIT_COMPLETE: AtomicBool = AtomicBool::new(false);

actions!(
    chatty,
    [
        OpenSettings,
        Quit,
        ToggleSidebar,
        NewConversation,
        PreviousConversation,
        NextConversation,
        DeleteActiveConversation,
        InstallCli
    ]
);

fn get_themes_dir() -> PathBuf {
    // Check CHATTY_DATA_DIR environment variable (set by AppImage)
    if let Ok(data_dir) = std::env::var("CHATTY_DATA_DIR") {
        let themes_path = PathBuf::from(data_dir).join("themes");
        if themes_path.exists() {
            return themes_path;
        }
    }

    // Try to find themes directory relative to the executable
    #[cfg(target_os = "macos")]
    {
        // On macOS, check in the app bundle's Resources directory
        if let Ok(exe_path) = std::env::current_exe()
            && let Some(app_bundle) = exe_path
                .ancestors()
                .find(|p| p.extension().map(|e| e == "app").unwrap_or(false))
        {
            let resources_themes = app_bundle.join("Contents/Resources/themes");
            if resources_themes.exists() {
                return resources_themes;
            }
        }
    }

    // Default to ./themes for development and Linux/Windows
    PathBuf::from("./themes")
}

fn init_themes(cx: &mut App) {
    let themes_dir = get_themes_dir();
    info!(themes_dir = ?themes_dir, "Loading themes from directory");

    // Just watch themes directory to load the registry
    if let Err(err) = ThemeRegistry::watch_dir(themes_dir, cx, |_cx| {
        // Empty callback - just loading themes into registry
    }) {
        warn!(error = ?err, "Failed to watch themes directory");
    }

    // Observe theme changes and persist base theme name + dark mode to GeneralSettingsModel
    // Only persist after initialization is complete to avoid overwriting saved preferences
    cx.observe_global::<Theme>(|cx| {
        // Skip saving during initialization - settings haven't been loaded yet
        if !THEME_INIT_COMPLETE.load(Ordering::SeqCst) {
            debug!("Skipping theme save during initialization");
            return;
        }

        let full_theme_name = cx.theme().theme_name().to_string();
        let is_dark = cx.theme().mode.is_dark();

        // Extract base theme name using shared utility
        let base_theme_name = settings::utils::extract_base_theme_name(&full_theme_name);

        // Update model with base name and dark mode
        {
            let settings = cx.global_mut::<settings::models::general_model::GeneralSettingsModel>();
            settings.theme_name = Some(base_theme_name);
            settings.dark_mode = Some(is_dark);
        }

        // Save async
        let settings = cx
            .global::<settings::models::general_model::GeneralSettingsModel>()
            .clone();
        cx.spawn(|_cx: &mut AsyncApp| async move {
            let repo = chatty_core::general_settings_repository();
            if let Err(e) = repo.save(settings).await {
                warn!(error = ?e, "Failed to save theme preference");
            }
        })
        .detach();
    })
    .detach();

    cx.refresh_windows();
}

/// Apply theme from saved settings (called after settings are loaded from JSON)
fn apply_theme_from_settings(cx: &mut App) {
    let base_theme_name = cx
        .global::<settings::models::general_model::GeneralSettingsModel>()
        .theme_name
        .clone()
        .unwrap_or_else(|| "Ayu".to_string());

    let is_dark = cx
        .global::<settings::models::general_model::GeneralSettingsModel>()
        .dark_mode
        .unwrap_or(false);

    info!(
        theme = %base_theme_name,
        dark_mode = is_dark,
        "Applying theme from saved settings"
    );

    // Find the appropriate theme variant using shared utility
    let full_theme_name = settings::utils::find_theme_variant(cx, &base_theme_name, is_dark);

    if let Some(theme) = ThemeRegistry::global(cx)
        .themes()
        .get(&full_theme_name)
        .cloned()
    {
        // Set the mode first
        let mode = if is_dark {
            ThemeMode::Dark
        } else {
            ThemeMode::Light
        };
        Theme::global_mut(cx).mode = mode;

        // Then apply the theme
        Theme::global_mut(cx).apply_config(&theme);
        cx.refresh_windows();

        info!(theme = %full_theme_name, "Theme applied successfully");
    } else {
        warn!(
            theme = %full_theme_name,
            "Theme not found in registry, keeping default"
        );
    }

    // Mark initialization complete - now the observer can save user changes
    THEME_INIT_COMPLETE.store(true, Ordering::SeqCst);
    debug!("Theme initialization complete, observer now active");
}

fn register_actions(cx: &mut App) {
    // Register open settings action with platform-specific keybindings
    debug!("Action registered");

    #[cfg(target_os = "macos")]
    cx.bind_keys([
        KeyBinding::new("cmd-,", OpenSettings, None),
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-b", ToggleSidebar, None),
        KeyBinding::new("cmd-n", NewConversation, None),
        KeyBinding::new("cmd-up", PreviousConversation, None),
        KeyBinding::new("cmd-down", NextConversation, None),
        KeyBinding::new("alt-backspace", DeleteActiveConversation, None),
        KeyBinding::new("cmd-backspace", DeleteActiveConversation, None),
    ]);

    #[cfg(not(target_os = "macos"))]
    cx.bind_keys([
        KeyBinding::new("ctrl-,", OpenSettings, None),
        KeyBinding::new("ctrl-q", Quit, None),
        KeyBinding::new("ctrl-b", ToggleSidebar, None),
        KeyBinding::new("ctrl-n", NewConversation, None),
        KeyBinding::new("ctrl-up", PreviousConversation, None),
        KeyBinding::new("ctrl-down", NextConversation, None),
        KeyBinding::new("ctrl-backspace", DeleteActiveConversation, None),
    ]);
    cx.on_action(|_: &OpenSettings, cx: &mut App| {
        debug!("Action triggered");
        SettingsView::open_or_focus_settings_window(cx);
    });
    cx.on_action(|_: &Quit, cx: &mut App| {
        debug!("Quit action triggered");
        chatty::services::cleanup_thumbnails();

        // Stop all active streams gracefully
        if let Some(manager) = cx
            .try_global::<chatty::models::GlobalStreamManager>()
            .and_then(|g| g.entity.clone())
        {
            manager.update(cx, |mgr, cx| {
                mgr.stop_all(cx);
            });
        }

        // Disconnect from all MCP servers on quit.
        let mcp_service = cx.global::<chatty::services::McpService>().clone();
        cx.spawn(|_cx: &mut AsyncApp| async move {
            if let Err(e) = mcp_service.disconnect_all().await {
                error!(error = ?e, "Failed to disconnect MCP servers during shutdown");
            }
        })
        .detach();

        // Mandatory auto-update: if an update has been downloaded and is ready,
        // install it silently before quitting so the next launch runs the new
        // version. install_on_quit() does NOT relaunch — the user intended to
        // quit, so we respect that while still ensuring the update is applied.
        if let Some(updater) = cx.try_global::<AutoUpdater>()
            && matches!(updater.status(), auto_updater::AutoUpdateStatus::Ready(..))
        {
            info!("Pending update found on quit — installing before exit");
            cx.update_global::<AutoUpdater, _>(|updater, cx| {
                updater.install_on_quit(cx);
            });
            return;
        }

        cx.quit();
    });
    cx.on_action(|_: &ToggleSidebar, cx: &mut App| {
        debug!("Toggle sidebar action triggered");
        with_chatty_app(cx, |app, cx| {
            app.sidebar_view.update(cx, |sidebar, cx| {
                sidebar.toggle_collapsed(cx);
            });
        });
    });
    cx.on_action(|_: &NewConversation, cx: &mut App| {
        debug!("New conversation action triggered");
        with_chatty_app(cx, |app, cx| {
            app.start_new_conversation(cx);
        });
    });
    cx.on_action(|_: &PreviousConversation, cx: &mut App| {
        debug!("Previous conversation action triggered");
        with_chatty_app(cx, |app, cx| {
            app.navigate_conversation(-1, cx);
        });
    });
    cx.on_action(|_: &NextConversation, cx: &mut App| {
        debug!("Next conversation action triggered");
        with_chatty_app(cx, |app, cx| {
            app.navigate_conversation(1, cx);
        });
    });
    cx.on_action(|_: &DeleteActiveConversation, cx: &mut App| {
        debug!("Delete active conversation action triggered");
        with_chatty_app(cx, |app, cx| {
            app.delete_active_conversation(cx);
        });
    });
    cx.on_action(|_: &InstallCli, cx: &mut App| {
        debug!("Install CLI action triggered");
        cli_installer::install_cli(cx);
    });
}

fn set_app_menus(cx: &mut App) {
    cx.set_menus(vec![Menu {
        name: "Chatty".into(),
        items: vec![
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action("Settings", OpenSettings),
            MenuItem::action("Toggle Sidebar", ToggleSidebar),
            MenuItem::separator(),
            MenuItem::action("Install CLI\u{2026}", InstallCli),
            MenuItem::separator(),
            MenuItem::action("Quit", Quit),
        ],
    }]);
}

fn main() {
    // Initialize error collector layer
    let (error_layer, error_receiver) = chatty::services::ErrorCollectorLayer::new();

    // Initialize structured logging with custom error collector layer
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .with(error_layer)
        .with(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .init();

    tracing::info!("Starting Chatty application");

    // Initialize Tokio runtime for rig LLM operations
    // rig requires Tokio 1.x runtime for async operations
    let _tokio_runtime = tokio::runtime::Runtime::new().expect("Failed to create Tokio runtime");

    // Enter the runtime context for the entire application
    // This allows async operations to use Tokio's runtime
    let _guard = _tokio_runtime.enter();

    // Initialize all settings repositories (providers, models, MCP, etc.).
    // This must happen before anything accesses the repository singletons.
    chatty_core::init_repositories()
        .expect("Failed to initialize settings repositories (is HOME set?)");

    // Initialize the SQLite conversation repository here, where the Tokio runtime is
    // explicitly set up, so the block_on call is clearly safe and in a known context.
    let conversation_repo: Arc<dyn ConversationRepository> = Arc::new(
        _tokio_runtime
            .block_on(ConversationSqliteRepository::new())
            .expect("Failed to create SQLite conversation repository"),
    );

    let app = Application::new()
        .with_assets(gpui_component_assets::Assets)
        .with_assets(ChattyAssets);

    app.run(move |cx| {
        cx.activate(true);

        // Initialize the theme
        init(cx);

        // Initialize general settings with default - will be populated async
        cx.set_global(settings::models::general_model::GeneralSettingsModel::default());

        // Initialize theme system
        init_themes(cx);

        // Load general settings asynchronously without blocking startup
        cx.spawn(async move |cx: &mut AsyncApp| {
            let repo = chatty_core::general_settings_repository();
            match repo.load().await {
                Ok(settings) => {
                    cx.update(|cx| {
                        // Update global settings
                        cx.set_global(settings);

                        // Apply theme from loaded settings
                        apply_theme_from_settings(cx);
                    })
                    .map_err(|e| warn!(error = ?e, "Failed to apply theme from loaded settings"))
                    .ok();
                }
                Err(e) => {
                    warn!(error = ?e, "Failed to load general settings, using default settings");
                    // Mark initialization complete even on error so the observer can save future changes
                    THEME_INIT_COMPLETE.store(true, Ordering::SeqCst);
                }
            }
        })
        .detach();

        // Initialize providers model with empty state - will be populated async
        cx.set_global(settings::models::ProviderModel::new());

        // Initialize models model with empty state - will be populated async
        cx.set_global(settings::models::ModelsModel::new());

        // Initialize MCP servers model with empty state - will be populated async
        cx.set_global(settings::models::McpServersModel::new());

        // Initialize execution settings with default - will be populated async
        cx.set_global(settings::models::ExecutionSettingsModel::default());

        // Initialize search settings with default - will be populated async
        cx.set_global(settings::models::SearchSettingsModel::default());

        // Initialize training settings with default - will be populated async
        cx.set_global(settings::models::TrainingSettingsModel::default());

        // Initialize user secrets with empty state - will be populated async
        cx.set_global(settings::models::UserSecretsModel::default());

        // Initialize module settings with default - will be populated async
        cx.set_global(settings::models::ModuleSettingsModel::default());
        cx.set_global(settings::models::DiscoveredModulesModel::default());

        // Initialize Hive/extensions globals
        cx.set_global(settings::models::HiveSettingsModel::default());
        cx.set_global(settings::models::ExtensionsModel::default());
        cx.set_global(settings::models::MarketplaceState::default());

        settings::controllers::module_settings_controller::refresh_runtime(cx);

        // Initialize agent memory service asynchronously.
        // A watch channel is stored as a global so that conversation creation can await
        // completion, preventing a race where the agent would be built without memory tools.
        let (memory_tx, memory_rx) = tokio::sync::watch::channel(false);
        cx.set_global(MemoryInitSignal(memory_rx));
        cx.spawn(async move |cx: &mut AsyncApp| {
            // Check if memory is enabled in settings
            let memory_enabled = cx
                .update(|cx| {
                    cx.try_global::<settings::models::ExecutionSettingsModel>()
                        .map(|s| s.memory_enabled)
                        .unwrap_or(true)
                })
                .unwrap_or(true);

            if memory_enabled {
                if let Some(data_dir) = chatty_core::services::memory_service::memory_data_dir() {
                    match chatty_core::services::MemoryService::open_or_create(&data_dir).await {
                        Ok(service) => {
                            cx.update(|cx| {
                                cx.set_global(service);
                                info!("Agent memory service initialized");
                            })
                            .map_err(|e| warn!(error = ?e, "Failed to set MemoryService global"))
                            .ok();
                        }
                        Err(e) => {
                            warn!(error = ?e, "Failed to initialize agent memory service");
                        }
                    }
                } else {
                    warn!("Could not determine data directory for agent memory");
                }
            } else {
                info!("Agent memory disabled by settings");
            }

            // Initialize embedding service for semantic memory search (if configured)
            let embedding_settings = cx
                .update(|cx| {
                    cx.try_global::<settings::models::ExecutionSettingsModel>()
                        .map(|s| (s.embedding_enabled, s.embedding_provider.clone(), s.embedding_model.clone()))
                })
                .ok()
                .flatten();

            if let Some((true, Some(embed_provider_type), Some(embed_model))) = embedding_settings {
                let provider_config = cx
                    .update(|cx| {
                        cx.try_global::<settings::models::ProviderModel>()
                            .and_then(|pm| {
                                pm.providers()
                                    .iter()
                                    .find(|p| p.provider_type == embed_provider_type)
                                    .cloned()
                            })
                    })
                    .ok()
                    .flatten();

                let api_key = provider_config.as_ref().and_then(|p| p.api_key.clone());
                let base_url = provider_config.as_ref().and_then(|p| p.base_url.clone());

                // Fetch Entra ID token if the Azure provider uses Entra ID auth
                let azure_token = if embed_provider_type == settings::models::providers_store::ProviderType::AzureOpenAI
                    && provider_config.as_ref().map(|p| p.azure_auth_method()) == Some(settings::models::providers_store::AzureAuthMethod::EntraId)
                {
                    match chatty_core::auth::azure_auth::fetch_entra_id_token().await {
                        Ok(token) => Some(token),
                        Err(e) => {
                            warn!(error = ?e, "Failed to fetch Entra ID token for Azure OpenAI embeddings");
                            None
                        }
                    }
                } else {
                    None
                };

                if let Some(embed_svc) = chatty_core::services::embedding_service::try_create_embedding_service(
                    &embed_provider_type,
                    &embed_model,
                    api_key.as_deref(),
                    base_url.as_deref(),
                    azure_token,
                ) {
                    // Enable vector index on memory service
                    let mem_svc = cx
                        .update(|cx| {
                            cx.try_global::<chatty_core::services::MemoryService>().cloned()
                        })
                        .ok()
                        .flatten();

                    if let Some(ref mem_svc) = mem_svc {
                        if let Err(e) = mem_svc.enable_vec().await {
                            warn!(error = ?e, "Failed to enable vector index on memory service");
                        } else if let Err(e) = mem_svc.set_vec_model(&embed_svc.model_identifier()).await {
                            warn!(error = ?e, "Failed to set vector model — falling back to BM25-only");
                        } else {
                            info!(model = %embed_svc.model_identifier(), "Vector search enabled for memory");
                        }
                    }

                    cx.update(|cx| {
                        let skill_service = chatty_core::services::SkillService::new(Some(embed_svc.clone()));
                        cx.set_global(embed_svc);
                        cx.set_global(skill_service);
                    })
                    .map_err(|e| warn!(error = ?e, "Failed to set EmbeddingService and SkillService globals"))
                    .ok();
                }
            }

            // Signal that memory init is done (success, failure, or disabled)
            let _ = memory_tx.send(true);
        })
        .detach();

        // Initialize execution approval store for tracking pending approvals
        cx.set_global(chatty::models::ExecutionApprovalStore::new());

        // Initialize write approval store for tracking filesystem write approvals
        cx.set_global(chatty::models::WriteApprovalStore::new());

        // Initialize SkillService global (no embedding initially — keyword-only scoring).
        // Replaced by a version with embedding once EmbeddingService is initialised above.
        cx.set_global(chatty_core::services::SkillService::new(None));

        // Initialize token tracking settings with defaults
        cx.set_global(settings::models::TokenTrackingSettings::default());

        // Initialize the token budget watch channel global used by the context bar
        cx.set_global(chatty::token_budget::GlobalTokenBudget::new());

        // Spawn background task to watch the token budget channel and trigger window refreshes.
        // This is necessary because token counting runs asynchronously in parallel with the LLM
        // call, and the snapshot arrives after the initial render. The watch channel update needs
        // to signal GPUI to re-render the context bar view.
        cx.spawn(async move |cx: &mut AsyncApp| {
            if let Some(global) = cx.update(|cx| {
                cx.try_global::<chatty::token_budget::GlobalTokenBudget>()
                    .map(|g| g.receiver.clone())
            }).ok().flatten() {
                let mut receiver = global;
                loop {
                    // Wait for any change in the watch channel
                    if receiver.changed().await.is_err() {
                        // Channel closed (app shutting down)
                        break;
                    }
                    // Trigger a window refresh when snapshot updates
                    cx.update(|cx| {
                        cx.refresh_windows();
                    })
                    .map_err(|e| warn!(error = ?e, "Failed to refresh windows for token snapshot"))
                    .ok();
                }
            }
        })
        .detach();

        // Initialize models notifier entity for event subscriptions
        let models_notifier = cx.new(|_cx| settings::models::ModelsNotifier::new());
        cx.set_global(settings::models::GlobalModelsNotifier {
            entity: Some(models_notifier.downgrade()),
        });

        // Create MCP update channel and spawn listener that updates global + emits event
        let (mcp_tx, mut mcp_rx) = tokio::sync::mpsc::channel::<
            Vec<settings::models::mcp_store::McpServerConfig>,
        >(64);
        MCP_UPDATE_SENDER.set(mcp_tx)
            .map_err(|_| warn!("MCP_UPDATE_SENDER already initialized"))
            .ok();

        cx.spawn(async move |cx: &mut AsyncApp| {
            while let Some(servers) = mcp_rx.recv().await {
                cx.update(|cx| {
                    // Legacy sync
                    cx.update_global::<settings::models::McpServersModel, _>(|model, _cx| {
                        model.replace_all(servers.clone());
                    });

                    // Sync into ExtensionsModel: add/update MCP servers from the
                    // tool-modified list while preserving non-MCP extensions.
                    {
                        use chatty_core::settings::models::extensions_store::{
                            ExtensionKind, ExtensionSource, ExtensionsModel, InstalledExtension,
                        };
                        let ext_model = cx.global_mut::<ExtensionsModel>();
                        for server in &servers {
                            let ext_id = format!("mcp-{}", server.name);
                            if let Some(ext) = ext_model.find_mut(&ext_id) {
                                ext.enabled = server.enabled;
                                ext.kind = ExtensionKind::McpServer(server.clone());
                            } else {
                                ext_model.add(InstalledExtension {
                                    id: ext_id,
                                    display_name: server.name.clone(),
                                    description: String::new(),
                                    kind: ExtensionKind::McpServer(server.clone()),
                                    source: ExtensionSource::Custom,
                                    enabled: server.enabled,
                                });
                            }
                        }
                        let ext_clone = ext_model.clone();
                        let repo = chatty_core::extensions_repository();
                        cx.spawn(|_cx: &mut AsyncApp| async move {
                            if let Err(e) = repo.save(ext_clone).await {
                                tracing::error!(error = ?e, "Failed to sync MCP update to extensions");
                            }
                        })
                        .detach();
                    }

                    if let Some(weak_notifier) = cx
                        .try_global::<settings::models::GlobalAgentConfigNotifier>()
                        .and_then(|g| g.entity.clone())
                        && let Some(notifier) = weak_notifier.upgrade()
                    {
                        notifier.update(cx, |_notifier, cx| {
                            cx.emit(settings::models::AgentConfigEvent::RebuildRequired);
                        });
                    }
                })
                .map_err(|e| warn!(error = ?e, "Failed to update MCP servers model"))
                .ok();
            }
        })
        .detach();

        // Initialize StreamManager entity for tracking active LLM streams per conversation
        // Store a strong Entity reference in the global to prevent garbage collection
        // when the initialization closure's local variables go out of scope.
        let stream_manager = cx.new(|_cx| chatty::models::StreamManager::new());
        cx.set_global(chatty::models::GlobalStreamManager {
            entity: Some(stream_manager),
        });

        // Initialize error store and notifier
        cx.set_global(chatty::models::ErrorStore::new(100)); // Max 100 entries

        let error_notifier = cx.new(|_cx| chatty::models::ErrorNotifier::new());
        cx.set_global(chatty::models::GlobalErrorNotifier {
            entity: Some(error_notifier.downgrade()),
        });

        // Spawn background thread to consume errors from tracing layer
        // Bridge sync channel to tokio channel
        let (error_tx, mut error_rx) = tokio::sync::mpsc::unbounded_channel();

        std::thread::spawn(move || {
            while let Ok(entry) = error_receiver.recv() {
                let _ = error_tx.send(entry);
            }
        });

        // Spawn async task to process errors on main thread
        cx.spawn(async move |cx: &mut AsyncApp| {
            while let Some(entry) = error_rx.recv().await {
                let _ = cx.update(|cx| {
                    // Add to global store
                    cx.update_global::<chatty::models::ErrorStore, _>(|store, _cx| {
                        store.add_entry(entry);
                    });

                    // Notify UI
                    if let Some(weak_notifier) = cx
                        .try_global::<chatty::models::GlobalErrorNotifier>()
                        .and_then(|g| g.entity.clone())
                        && let Some(notifier) = weak_notifier.upgrade()
                    {
                        notifier.update(cx, |_notifier, cx| {
                            cx.emit(chatty::models::ErrorNotifierEvent::NewError);
                        });
                    }

                    // Refresh windows to update badge count
                    cx.refresh_windows();
                });
            }
        })
        .detach();

        // Initialize global settings window state
        cx.set_global(settings::controllers::GlobalSettingsWindow::default());

        // Initialize global models list view state
        cx.set_global(settings::views::models_page::GlobalModelsListView::default());

        // Initialize auto-updater with current version from Cargo.toml
        let updater = AutoUpdater::new(env!("CARGO_PKG_VERSION"));
        cx.set_global(updater.clone());

        // Check if a previous update installation failed (macOS only)
        updater.check_previous_update_status(cx);

        updater.start_polling(cx);
        info!("Auto-updater initialized and polling started");

        // Silently refresh the CLI binary if it was previously installed.
        // On Linux this re-copies chatty-tui after an AppImage auto-update;
        // on macOS/Windows the symlink / installer already keeps it in sync.
        cli_installer::update_cli_if_installed(cx);

        // Initialize math renderer service for LaTeX math rendering
        let math_renderer = chatty::services::MathRendererService::new();
        cx.set_global(math_renderer);
        info!("Math renderer service initialized");

        // Clean up old styled SVG files from previous sessions
        if let Err(e) = chatty::services::MathRendererService::cleanup_old_styled_svgs() {
            warn!(error = ?e, "Failed to cleanup old math SVG files");
        }

        // Initialize mermaid renderer service for diagram rendering
        let mermaid_renderer = chatty::services::MermaidRendererService::new();
        cx.set_global(mermaid_renderer);
        info!("Mermaid renderer service initialized");

        // Clean up old mermaid SVG files from previous sessions
        if let Err(e) = chatty::services::MermaidRendererService::cleanup_old_svgs() {
            warn!(error = ?e, "Failed to cleanup old mermaid SVG files");
        }

        // Augment PATH for GUI app launch — macOS/Linux .app bundles don't inherit the
        // shell PATH, so executables like npx, uvx, az, etc. won't be found otherwise.
        chatty::auth::azure_auth::augment_gui_app_path();

        // Initialize MCP service for managing MCP server connections
        let mcp_service = chatty::services::McpService::new();
        MCP_SERVICE.set(mcp_service.clone())
            .map_err(|_| warn!("MCP_SERVICE already initialized"))
            .ok();
        cx.set_global(mcp_service);
        info!("MCP service initialized");

        // Load providers, models, and execution settings concurrently (dependency tier 1).
        // Conversations depend on all three being loaded (dependency tier 2).
        // Using tokio::join! makes the dependency graph explicit and eliminates AtomicBool polling.
        cx.spawn(async move |cx: &mut AsyncApp| {
            // Run all three I/O operations in parallel before touching global state
            let (providers_result, models_result, exec_settings_result, search_settings_result) = tokio::join!(
                chatty_core::provider_repository().load_all(),
                chatty_core::models_repository().load_all(),
                chatty_core::execution_settings_repository().load(),
                chatty_core::search_settings_repository().load(),
            );

            // Apply providers result
            match providers_result {
                Ok(providers) => {
                    cx.update(|cx| {
                        cx.update_global::<settings::models::ProviderModel, _>(|model, _cx| {
                            model.replace_all(providers);
                        });

                        // Ensure Ollama provider exists with default settings
                        let should_save = settings::providers::ensure_default_ollama_provider(cx);

                        // Save the updated providers if we added Ollama
                        if should_save {
                            let providers_to_save = cx
                                .global::<settings::models::ProviderModel>()
                                .providers()
                                .to_vec();
                            let repo_for_save = chatty_core::provider_repository();
                            cx.spawn(|_cx: &mut AsyncApp| async move {
                                if let Err(e) = repo_for_save.save_all(providers_to_save).await {
                                    warn!(error = ?e, "Failed to persist default Ollama provider");
                                }
                            })
                            .detach();
                        }

                        info!("Providers loaded");

                        // Initialize Azure token cache if any providers use Entra ID
                        let needs_cache = cx
                            .global::<settings::models::ProviderModel>()
                            .providers()
                            .iter()
                            .any(|p| {
                                p.provider_type == settings::models::providers_store::ProviderType::AzureOpenAI
                                    && p.azure_auth_method() == settings::models::providers_store::AzureAuthMethod::EntraId
                            });

                        if needs_cache {
                            tracing::info!("Pre-initializing Azure token cache");
                            cx.spawn(|_cx: &mut AsyncApp| async move {
                                if let Ok(cache) = chatty::auth::AzureTokenCache::new() {
                                    // Pre-warm cache with initial token
                                    if let Err(e) = cache.get_token().await {
                                        tracing::warn!(error = ?e, "Failed to pre-fetch Azure token");
                                    } else {
                                        tracing::info!("Azure token cache pre-warmed successfully");
                                    }

                                    // Note: Cache is also lazily initialized in agent_factory.rs
                                    // This pre-warming is just an optimization to avoid first-message delay
                                }
                            })
                            .detach();
                        }
                    })
                    .map_err(|e| warn!(error = ?e, "Failed to update global providers after load"))
                    .ok();
                }
                Err(e) => {
                    error!(error = ?e, "Failed to load providers");
                }
            }

            // Apply models result (providers are now applied, so ProviderModel is up to date for Ollama URL)
            match models_result {
                Ok(models) => {
                    cx.update(|cx| {
                        cx.update_global::<settings::models::ModelsModel, _>(|model, _cx| {
                            // Apply default capabilities for models that don't have them set
                            let models: Vec<_> = models
                                .into_iter()
                                .map(|mut m| {
                                    if !m.supports_images && !m.supports_pdf {
                                        let (img, pdf) = m.provider_type.default_capabilities();
                                        m.supports_images = img;
                                        m.supports_pdf = pdf;
                                    }
                                    m
                                })
                                .collect();
                            model.replace_all(models);
                        });

                        // Emit ModelsReady event for subscribers
                        if let Some(weak_notifier) = cx
                            .try_global::<settings::models::GlobalModelsNotifier>()
                            .and_then(|g| g.entity.clone())
                            && let Some(notifier) = weak_notifier.upgrade()
                        {
                            notifier.update(cx, |_notifier, cx| {
                                cx.emit(settings::models::ModelsNotifierEvent::ModelsReady);
                            });
                        }
                        info!("Models loaded");

                        // Refresh the module runtime so the gateway gets a real
                        // HostLlmProvider now that models/providers are available.
                        // The initial refresh_runtime() call at startup runs before
                        // providers finish loading asynchronously, which causes the
                        // gateway to use the noop provider.
                        settings::controllers::module_settings_controller::refresh_runtime(cx);

                        // Always attempt to auto-discover Ollama models on startup
                        debug!("Attempting Ollama model auto-discovery");

                        // Get Ollama base URL
                        let ollama_base_url = cx
                            .global::<settings::models::ProviderModel>()
                            .providers()
                            .iter()
                            .find(|p| {
                                matches!(
                                    p.provider_type,
                                    settings::models::providers_store::ProviderType::Ollama
                                )
                            })
                            .and_then(|p| p.base_url.clone())
                            .unwrap_or_else(|| "http://localhost:11434".to_string());

                        cx.spawn(async move |cx: &mut AsyncApp| {
                            settings::providers::sync_ollama_models(&ollama_base_url, cx)
                                .await
                                .map_err(|e| warn!(error = ?e, "Failed to sync Ollama models"))
                                .ok();
                        })
                        .detach();

                        // Refresh all chat inputs with newly loaded models
                        cx.refresh_windows();
                    })
                    .map_err(|e| warn!(error = ?e, "Failed to update models after load"))
                    .ok();
                }
                Err(e) => {
                    error!(error = ?e, "Failed to load models");
                }
            }

            // Apply execution settings result
            match exec_settings_result {
                Ok(settings) => {
                    cx.update(|cx| {
                        info!(
                            enabled = settings.enabled,
                            workspace = ?settings.workspace_dir,
                            approval_mode = ?settings.approval_mode,
                            network_isolation = settings.network_isolation,
                            "Execution settings loaded from disk"
                        );
                        cx.set_global(settings);
                    })
                    .map_err(|e| warn!(error = ?e, "Failed to update global execution settings"))
                    .ok();
                }
                Err(e) => {
                    warn!(error = ?e, "Failed to load execution settings, using defaults");
                    // Defaults will be used (enabled=false); conversations will still load
                }
            }

            // Apply search settings result
            match search_settings_result {
                Ok(settings) => {
                    cx.update(|cx| {
                        info!(
                            enabled = settings.enabled,
                            provider = ?settings.active_provider,
                            "Search settings loaded from disk"
                        );
                        cx.set_global(settings);
                    })
                    .map_err(|e| warn!(error = ?e, "Failed to update global search settings"))
                    .ok();
                }
                Err(e) => {
                    warn!(error = ?e, "Failed to load search settings, using defaults");
                }
            }

            // All tier-1 loads complete; trigger conversation loading
            info!("Models, providers, and execution settings loaded, triggering conversation load");
            cx.update(|cx| {
                with_chatty_app(cx, |app, cx| {
                    app.load_conversations_after_models_ready(cx);
                });
            })
            .map_err(|e| warn!(error = ?e, "Failed to trigger conversation load"))
            .ok();
        })
        .detach();

        // Load MCP server configurations asynchronously without blocking startup
        cx.spawn(async move |cx: &mut AsyncApp| {
            let repo = chatty_core::mcp_repository();
            match repo.load_all().await {
                Ok(servers) => {
                    let servers_clone = servers.clone();
                    cx.update(|cx| {
                        cx.update_global::<settings::models::McpServersModel, _>(|model, _cx| {
                            model.replace_all(servers);
                        });
                        info!("MCP server configurations loaded");

                        // Merge legacy MCP servers into ExtensionsModel so it
                        // reflects servers that were only in mcp_servers.json.
                        {
                            use chatty_core::settings::models::extensions_store::{
                                ExtensionKind, ExtensionSource, InstalledExtension,
                            };
                            let ext_model = cx.global_mut::<settings::models::ExtensionsModel>();
                            for server in &servers_clone {
                                let ext_id = format!("mcp-{}", server.name);
                                if ext_model.find(&ext_id).is_none() {
                                    ext_model.add(InstalledExtension {
                                        id: ext_id,
                                        display_name: server.name.clone(),
                                        description: String::new(),
                                        kind: ExtensionKind::McpServer(server.clone()),
                                        source: ExtensionSource::Custom,
                                        enabled: server.enabled,
                                    });
                                }
                            }
                        }

                        // Inject the stored Hive JWT token into the "hive" MCP
                        // server config so authenticated tool calls work on
                        // startup without requiring an extra login.
                        let mut servers_clone = servers_clone;
                        if let Some(token) = cx
                            .global::<chatty_core::settings::models::hive_settings::HiveSettingsModel>()
                            .token
                            .clone()
                            && let Some(hive_cfg) =
                                servers_clone.iter_mut().find(|s| s.name == "hive")
                            && hive_cfg.api_key.as_ref().is_none_or(|k| k.is_empty())
                        {
                            hive_cfg.api_key = Some(token);
                            info!("Injected Hive token into MCP server config for startup");
                        }

                        // Connect to all enabled MCP servers and track auth status
                        let mcp_service = cx.global::<chatty::services::McpService>().clone();
                        cx.spawn(async move |cx: &mut AsyncApp| {
                            let results =
                                mcp_service.connect_all_with_status(servers_clone).await;

                            cx.update(|cx| {
                                use settings::models::mcp_store::McpAuthStatus;

                                // Derive status once per connection result, then
                                // write to both models (ExtensionsModel is
                                // canonical; McpServersModel kept for legacy).
                                let statuses: Vec<(String, McpAuthStatus)> = results
                                    .into_iter()
                                    .map(|(name, ok, err_msg)| {
                                        let status = if ok {
                                            McpAuthStatus::Authenticated
                                        } else if let Some(ref msg) = err_msg {
                                            if msg.contains("Auth required")
                                                || msg.contains("AuthRequired")
                                            {
                                                McpAuthStatus::NeedsAuth
                                            } else {
                                                McpAuthStatus::Failed(msg.clone())
                                            }
                                        } else {
                                            McpAuthStatus::Failed("Unknown error".to_string())
                                        };
                                        (name, status)
                                    })
                                    .collect();

                                let model = cx.global_mut::<settings::models::McpServersModel>();
                                for (name, status) in &statuses {
                                    model.set_auth_status(name.clone(), status.clone());
                                }

                                let ext_model = cx.global_mut::<settings::models::ExtensionsModel>();
                                for (name, status) in statuses {
                                    ext_model.set_mcp_auth_status(name, status);
                                }

                                cx.refresh_windows();
                            })
                            .map_err(|e| warn!(error = ?e, "Failed to update MCP auth statuses"))
                            .ok();
                        })
                        .detach();
                    })
                    .map_err(
                        |e| warn!(error = ?e, "Failed to update global MCP servers after load"),
                    )
                    .ok();
                }
                Err(e) => {
                    warn!(error = ?e, "Failed to load MCP server configurations");
                }
            }
        })
        .detach();

        // Load A2A agent configurations asynchronously without blocking startup
        cx.spawn(async move |cx: &mut AsyncApp| {
            let repo = chatty_core::a2a_repository();
            match repo.load_all().await {
                Ok(agents) => {
                    let names: Vec<String> = agents.iter().filter(|a| a.enabled).map(|a| a.name.clone()).collect();
                    cx.update(|cx| {
                        info!(count = names.len(), "A2A agent configurations loaded");

                        // Merge legacy A2A agents into ExtensionsModel
                        {
                            use chatty_core::settings::models::extensions_store::{
                                ExtensionKind, ExtensionSource, InstalledExtension,
                            };
                            let ext_model = cx.global_mut::<settings::models::ExtensionsModel>();
                            for agent in &agents {
                                let ext_id = format!("a2a-{}", agent.name);
                                if ext_model.find(&ext_id).is_none() {
                                    ext_model.add(InstalledExtension {
                                        id: ext_id,
                                        display_name: agent.name.clone(),
                                        description: String::new(),
                                        kind: ExtensionKind::A2aAgent(agent.clone()),
                                        source: ExtensionSource::Custom,
                                        enabled: agent.enabled,
                                    });
                                }
                            }
                        }

                        cx.refresh_windows();
                    })
                    .map_err(|e| warn!(error = ?e, "Failed to merge A2A agents into ExtensionsModel"))
                    .ok();

                    // Probe agent cards for all enabled agents in the background
                    for name in names {
                        let ext_id = format!("a2a-{name}");
                        cx.update(|cx| {
                            settings::controllers::a2a_controller::probe_agent_card(ext_id, name, cx);
                        })
                        .ok();
                    }
                }
                Err(e) => {
                    warn!(error = ?e, "Failed to load A2A agent configurations");
                }
            }
        })
        .detach();

        // Load training settings asynchronously (non-blocking, no dependencies)
        cx.spawn(async move |cx: &mut AsyncApp| {
            let repo = chatty_core::training_settings_repository();
            match repo.load().await {
                Ok(settings) => {
                    cx.update(|cx| {
                        info!(
                            atif_auto_export = settings.atif_auto_export,
                            "Training settings loaded from disk"
                        );
                        cx.set_global(settings);
                    })
                    .map_err(|e| warn!(error = ?e, "Failed to update global training settings"))
                    .ok();
                }
                Err(e) => {
                    warn!(error = ?e, "Failed to load training settings, using defaults");
                }
            }
        })
        .detach();

        // Load user secrets asynchronously
        cx.spawn(async move |cx: &mut AsyncApp| {
            let repo = chatty_core::user_secrets_repository();
            match repo.load().await {
                Ok(secrets) => {
                    let count = secrets.secrets.len();
                    cx.update(|cx| {
                        info!(count, "User secrets loaded from disk");
                        cx.set_global(secrets);
                    })
                    .map_err(|e| warn!(error = ?e, "Failed to update global user secrets"))
                    .ok();
                }
                Err(e) => {
                    warn!(error = ?e, "Failed to load user secrets, using defaults");
                }
            }
        })
        .detach();

        // Load module settings asynchronously
        cx.spawn(async move |cx: &mut AsyncApp| {
            let repo = chatty_core::module_settings_repository();
            match repo.load().await {
                Ok(settings) => {
                    cx.update(|cx| {
                        info!(
                            enabled = settings.enabled,
                            port = settings.gateway_port,
                            dir = %settings.module_dir,
                            "Module settings loaded from disk"
                        );
                        cx.set_global(settings);
                        settings::controllers::module_settings_controller::refresh_runtime(cx);
                    })
                    .map_err(|e| warn!(error = ?e, "Failed to update global module settings"))
                    .ok();
                }
                Err(e) => {
                    warn!(error = ?e, "Failed to load module settings, using defaults");
                }
            }
        })
        .detach();

        // Load Hive settings and extensions asynchronously
        cx.spawn(async move |cx: &mut AsyncApp| {
            let (hive_result, ext_result) = tokio::join!(
                chatty_core::hive_settings_repository().load(),
                chatty_core::extensions_repository().load(),
            );

            if let Ok(hive) = hive_result {
                cx.update(|cx| {
                    info!(
                        registry = %hive.registry_url,
                        logged_in = hive.token.is_some(),
                        "Hive settings loaded"
                    );
                    cx.set_global(hive);
                })
                .ok();
            }

            if let Ok(ext) = ext_result {
                let count = ext.extensions.len();
                cx.update(|cx| {
                    info!(count, "Extensions loaded from disk");
                    cx.set_global(ext);
                })
                .ok();
            }

            // Sync hive_settings.registry_url from the Hive MCP extension URL.
            // extensions.json may have been edited to point at a different host
            // while hive_settings.json was absent (defaulting to localhost:8080).
            cx.update(|cx| {
                let ext_model = cx.global::<settings::models::ExtensionsModel>();
                if let Some(ext) = ext_model.find(chatty_core::install::HIVE_MCP_EXT_ID)
                    && let settings::models::extensions_store::ExtensionKind::McpServer(ref cfg) =
                        ext.kind
                {
                        let derived = cfg.url.trim_end_matches("/mcp").to_string();
                        let hive = cx.global::<settings::models::HiveSettingsModel>();
                        if !derived.is_empty() && derived != hive.registry_url {
                            info!(
                                old = %hive.registry_url,
                                new = %derived,
                                "Syncing Hive registry URL from extension"
                            );
                            let mut updated = hive.clone();
                            updated.registry_url = derived;
                            cx.set_global(updated.clone());
                            cx.spawn(|_cx: &mut AsyncApp| async move {
                                let _ = chatty_core::hive_settings_repository()
                                    .save(updated)
                                    .await;
                            })
                            .detach();
                        }
                }
            })
            .ok();

            // Ensure the built-in Hive MCP server extension exists
            cx.update(|cx| {
                let added =
                    settings::controllers::extensions_controller::ensure_default_hive_mcp(cx);
                if added {
                    // Persist the new extension and MCP server entries
                    let ext_model = cx.global::<settings::models::ExtensionsModel>().clone();
                    let mcp_servers = cx
                        .global::<settings::models::McpServersModel>()
                        .servers()
                        .to_vec();
                    cx.spawn(|_cx: &mut AsyncApp| async move {
                        let _ = chatty_core::extensions_repository().save(ext_model).await;
                        let _ = chatty_core::mcp_repository().save_all(mcp_servers).await;
                    })
                    .detach();
                }
            })
            .ok();
        })
        .detach();

        // register actions
        register_actions(cx);

        // Set up native macOS menu bar
        set_app_menus(cx);

        // Get platform-specific window options for main window
        let options = settings::utils::window_utils::get_main_window_options();

        let repo = conversation_repo.clone();
        cx.open_window(options, |window, cx| {
            let view = cx.new(|cx| ChattyApp::new(window, cx, repo.clone()));

            cx.new(|cx| Root::new(view, window, cx))
        })
        .expect("Failed to open main window");
    });
}
