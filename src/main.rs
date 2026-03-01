// Hide console window on Windows in release builds
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use gpui::*;
use gpui_component::*;
use tracing::{debug, error, info, warn};

mod assets;
mod auto_updater;
mod chatty;
mod settings;

use assets::ChattyAssets;
use auto_updater::AutoUpdater;
use chatty::repositories::{ConversationRepository, ConversationSqliteRepository};
use chatty::{ChattyApp, GlobalChattyApp};
use settings::SettingsView;
use settings::repositories::{
    ExecutionSettingsJsonRepository, ExecutionSettingsRepository, GeneralSettingsJsonRepository,
    GeneralSettingsRepository, JsonFileRepository, JsonMcpRepository, JsonModelsRepository,
    McpRepository, ModelsRepository, ProviderRepository, TrainingSettingsJsonRepository,
    TrainingSettingsRepository, UserSecretsJsonRepository, UserSecretsRepository,
};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

/// Flag to prevent theme observer from saving during initialization.
/// This avoids a race condition where the default theme could overwrite
/// the user's saved theme preference before settings are loaded.
static THEME_INIT_COMPLETE: AtomicBool = AtomicBool::new(false);

/// Sender half of the MCP update channel. Initialized once at startup.
/// AddMcpTool sends the updated server list here after a successful save.
pub static MCP_UPDATE_SENDER: OnceLock<
    tokio::sync::mpsc::Sender<Vec<settings::models::mcp_store::McpServerConfig>>,
> = OnceLock::new();

/// McpService instance accessible from tool context (no GPUI available there).
pub static MCP_SERVICE: OnceLock<chatty::services::McpService> = OnceLock::new();

actions!(
    chatty,
    [
        OpenSettings,
        Quit,
        ToggleSidebar,
        NewConversation,
        PreviousConversation,
        NextConversation,
        DeleteActiveConversation
    ]
);

// Global repositories
lazy_static::lazy_static! {
    static ref PROVIDER_REPOSITORY: Arc<dyn ProviderRepository> = {
        let repo = JsonFileRepository::new()
            .expect("Failed to initialize provider repository");
        Arc::new(repo)
    };

    static ref GENERAL_SETTINGS_REPOSITORY: Arc<dyn GeneralSettingsRepository> = {
        let repo = GeneralSettingsJsonRepository::new()
            .expect("Failed to initialize general settings repository");
        Arc::new(repo)
    };

    static ref MODELS_REPOSITORY: Arc<dyn ModelsRepository> = {
        let repo = JsonModelsRepository::new()
            .expect("Failed to initialize models repository");
        Arc::new(repo)
    };

    static ref MCP_REPOSITORY: Arc<dyn McpRepository> = {
        let repo = JsonMcpRepository::new()
            .expect("Failed to initialize MCP repository");
        Arc::new(repo)
    };

    static ref EXECUTION_SETTINGS_REPOSITORY: Arc<dyn ExecutionSettingsRepository> = {
        let repo = ExecutionSettingsJsonRepository::new()
            .expect("Failed to initialize execution settings repository");
        Arc::new(repo)
    };

    static ref TRAINING_SETTINGS_REPOSITORY: Arc<dyn TrainingSettingsRepository> = {
        let repo = TrainingSettingsJsonRepository::new()
            .expect("Failed to initialize training settings repository");
        Arc::new(repo)
    };

    static ref USER_SECRETS_REPOSITORY: Arc<dyn UserSecretsRepository> = {
        let repo = UserSecretsJsonRepository::new()
            .expect("Failed to initialize user secrets repository");
        Arc::new(repo)
    };

}

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
            let repo = GENERAL_SETTINGS_REPOSITORY.clone();
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

        // Shutdown all MCP servers.
        // kill_all_sync() sends SIGTERM synchronously to all child processes before
        // the process exits, preventing orphaned MCP server processes. The async
        // stop_all() provides a best-effort graceful shutdown attempt.
        let mcp_service = cx.global::<chatty::services::McpService>().clone();
        mcp_service.kill_all_sync();
        cx.spawn(|_cx: &mut AsyncApp| async move {
            if let Err(e) = mcp_service.stop_all().await {
                error!(error = ?e, "Failed to stop MCP servers during shutdown");
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
        // Get the ChattyApp entity and toggle the sidebar
        if let Some(weak_entity) = cx
            .try_global::<GlobalChattyApp>()
            .and_then(|global| global.entity.clone())
            && let Some(entity) = weak_entity.upgrade()
        {
            entity.update(cx, |app, cx| {
                app.sidebar_view.update(cx, |sidebar, cx| {
                    sidebar.toggle_collapsed(cx);
                });
            });
        }
    });
    cx.on_action(|_: &NewConversation, cx: &mut App| {
        debug!("New conversation action triggered");
        if let Some(weak_entity) = cx
            .try_global::<GlobalChattyApp>()
            .and_then(|global| global.entity.clone())
            && let Some(entity) = weak_entity.upgrade()
        {
            entity.update(cx, |app, cx| {
                app.start_new_conversation(cx);
            });
        }
    });
    cx.on_action(|_: &PreviousConversation, cx: &mut App| {
        debug!("Previous conversation action triggered");
        if let Some(weak_entity) = cx
            .try_global::<GlobalChattyApp>()
            .and_then(|global| global.entity.clone())
            && let Some(entity) = weak_entity.upgrade()
        {
            entity.update(cx, |app, cx| {
                app.navigate_conversation(-1, cx);
            });
        }
    });
    cx.on_action(|_: &NextConversation, cx: &mut App| {
        debug!("Next conversation action triggered");
        if let Some(weak_entity) = cx
            .try_global::<GlobalChattyApp>()
            .and_then(|global| global.entity.clone())
            && let Some(entity) = weak_entity.upgrade()
        {
            entity.update(cx, |app, cx| {
                app.navigate_conversation(1, cx);
            });
        }
    });
    cx.on_action(|_: &DeleteActiveConversation, cx: &mut App| {
        debug!("Delete active conversation action triggered");
        if let Some(weak_entity) = cx
            .try_global::<GlobalChattyApp>()
            .and_then(|global| global.entity.clone())
            && let Some(entity) = weak_entity.upgrade()
        {
            entity.update(cx, |app, cx| {
                app.delete_active_conversation(cx);
            });
        }
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
            let repo = GENERAL_SETTINGS_REPOSITORY.clone();
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

        // Initialize training settings with default - will be populated async
        cx.set_global(settings::models::TrainingSettingsModel::default());

        // Initialize user secrets with empty state - will be populated async
        cx.set_global(settings::models::UserSecretsModel::default());

        // Initialize execution approval store for tracking pending approvals
        cx.set_global(chatty::models::ExecutionApprovalStore::new());

        // Initialize write approval store for tracking filesystem write approvals
        cx.set_global(chatty::models::WriteApprovalStore::new());

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
                    cx.update_global::<settings::models::McpServersModel, _>(|model, _cx| {
                        model.replace_all(servers);
                    });

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

        // Initialize math renderer service for LaTeX math rendering
        let math_renderer = chatty::services::MathRendererService::new();
        cx.set_global(math_renderer);
        info!("Math renderer service initialized");

        // Clean up old styled SVG files from previous sessions
        if let Err(e) = chatty::services::MathRendererService::cleanup_old_styled_svgs() {
            warn!(error = ?e, "Failed to cleanup old math SVG files");
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

        // Use Arc<AtomicBool> to track when providers, models, and execution settings are loaded

        let providers_loaded = Arc::new(AtomicBool::new(false));
        let models_loaded = Arc::new(AtomicBool::new(false));
        let exec_settings_loaded = Arc::new(AtomicBool::new(false));

        // Helper function to check if all three are loaded and trigger conversation loading.
        // Execution settings must be loaded before conversations are created so that the
        // factory sees the real `enabled` flag (not the default `false`).
        let check_and_load_conversations = {
            let providers_loaded = providers_loaded.clone();
            let models_loaded = models_loaded.clone();
            let exec_settings_loaded = exec_settings_loaded.clone();
            move |cx: &mut App| {
                if providers_loaded.load(Ordering::SeqCst)
                    && models_loaded.load(Ordering::SeqCst)
                    && exec_settings_loaded.load(Ordering::SeqCst)
                {
                    info!("Models, providers, and execution settings loaded, triggering conversation load");

                    // Get the ChattyApp entity and call load_conversations_after_models_ready
                    if let Some(weak_entity) = cx
                        .try_global::<GlobalChattyApp>()
                        .and_then(|global| global.entity.clone())
                        && let Some(entity) = weak_entity.upgrade()
                    {
                        entity.update(cx, |app, cx| {
                            app.load_conversations_after_models_ready(cx);
                        });
                    }
                }
            }
        };

        // Load providers asynchronously without blocking startup
        let check_fn_1 = check_and_load_conversations.clone();
        let providers_loaded_clone = providers_loaded.clone();
        cx.spawn(async move |cx: &mut AsyncApp| {
            let repo = PROVIDER_REPOSITORY.clone();
            match repo.load_all().await {
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
                            let repo_for_save = repo.clone();
                            cx.spawn(|_cx: &mut AsyncApp| async move {
                                if let Err(e) = repo_for_save.save_all(providers_to_save).await {
                                    warn!(error = ?e, "Failed to persist default Ollama provider");
                                }
                            })
                            .detach();
                        }

                        providers_loaded_clone.store(true, Ordering::SeqCst);
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

                        check_fn_1(cx);
                    })
                    .map_err(|e| warn!(error = ?e, "Failed to update global providers after load"))
                    .ok();
                }
                Err(e) => {
                    error!(error = ?e, "Failed to load providers");
                }
            }
        })
        .detach();

        // Load models asynchronously without blocking startup
        let check_fn_2 = check_and_load_conversations.clone();
        let check_fn_3 = check_and_load_conversations;
        let models_loaded_clone = models_loaded.clone();
        cx.spawn(async move |cx: &mut AsyncApp| {
            let repo = MODELS_REPOSITORY.clone();
            match repo.load_all().await {
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

                        models_loaded_clone.store(true, Ordering::SeqCst);

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

                        check_fn_2(cx);
                    })
                    .map_err(|e| warn!(error = ?e, "Failed to update models after load"))
                    .ok();
                }
                Err(e) => {
                    error!(error = ?e, "Failed to load models");
                }
            }
        })
        .detach();

        // Load MCP server configurations asynchronously without blocking startup
        cx.spawn(async move |cx: &mut AsyncApp| {
            let repo = MCP_REPOSITORY.clone();
            match repo.load_all().await {
                Ok(servers) => {
                    let servers_clone = servers.clone();
                    cx.update(|cx| {
                        cx.update_global::<settings::models::McpServersModel, _>(|model, _cx| {
                            model.replace_all(servers);
                        });
                        info!("MCP server configurations loaded");

                        // Start all enabled MCP servers
                        let mcp_service = cx.global::<chatty::services::McpService>().clone();
                        cx.spawn(|_cx: &mut AsyncApp| async move {
                            if let Err(e) = mcp_service.start_all(servers_clone).await {
                                error!(error = ?e, "Failed to start MCP servers");
                            }
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

        // Load execution settings asynchronously without blocking startup.
        // Must complete before conversations are loaded so the factory sees the
        // real `enabled` flag instead of the default `false`.
        let exec_settings_loaded_clone = exec_settings_loaded.clone();
        cx.spawn(async move |cx: &mut AsyncApp| {
            let repo = EXECUTION_SETTINGS_REPOSITORY.clone();
            match repo.load().await {
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
                        exec_settings_loaded_clone.store(true, Ordering::SeqCst);
                        check_fn_3(cx);
                    })
                    .map_err(|e| warn!(error = ?e, "Failed to update global execution settings"))
                    .ok();
                }
                Err(e) => {
                    warn!(error = ?e, "Failed to load execution settings, using defaults");
                    // Still mark as loaded so conversations aren't blocked forever
                    // (defaults will be used: enabled=false)
                    cx.update(|cx| {
                        exec_settings_loaded_clone.store(true, Ordering::SeqCst);
                        check_fn_3(cx);
                    })
                    .map_err(|e| {
                        warn!(
                            error = ?e,
                            "Failed to trigger conversation load after exec settings error"
                        )
                    })
                    .ok();
                }
            }
        })
        .detach();

        // Load training settings asynchronously (non-blocking, no dependencies)
        cx.spawn(async move |cx: &mut AsyncApp| {
            let repo = TRAINING_SETTINGS_REPOSITORY.clone();
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
            let repo = USER_SECRETS_REPOSITORY.clone();
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
