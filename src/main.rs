use gpui::*;
use gpui_component::*;
use tracing::{debug, error, info, warn};

mod assets;
mod auto_updater;
mod chatty;
mod settings;

use assets::ChattyAssets;
use auto_updater::AutoUpdater;
use chatty::{ChattyApp, GlobalChattyApp};
use settings::SettingsView;
use settings::repositories::{
    GeneralSettingsJsonRepository, GeneralSettingsRepository, JsonFileRepository,
    JsonModelsRepository, ModelsRepository, ProviderRepository,
};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Flag to prevent theme observer from saving during initialization.
/// This avoids a race condition where the default theme could overwrite
/// the user's saved theme preference before settings are loaded.
static THEME_INIT_COMPLETE: AtomicBool = AtomicBool::new(false);

actions!(chatty, [OpenSettings, Quit]);

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
}

fn init_themes(cx: &mut App) {
    // Just watch themes directory to load the registry
    if let Err(err) = ThemeRegistry::watch_dir(PathBuf::from("./themes"), cx, |_cx| {
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
    // Register open settings action
    debug!("Action registered");
    cx.bind_keys([
        KeyBinding::new("cmd-,", OpenSettings, None),
        KeyBinding::new("cmd-q", Quit, None),
    ]);
    cx.on_action(|_: &OpenSettings, cx: &mut App| {
        debug!("Action triggered");
        SettingsView::open_or_focus_settings_window(cx);
    });
    cx.on_action(|_: &Quit, cx: &mut App| {
        debug!("Quit action triggered");
        cx.quit();
    });
}

fn main() {
    // Initialize structured logging
    tracing_subscriber::fmt()
        .with_env_filter(
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

        // Initialize global settings window state
        cx.set_global(settings::controllers::GlobalSettingsWindow::default());

        // Initialize global models list view state
        cx.set_global(settings::views::models_page::GlobalModelsListView::default());

        // Initialize auto-updater with current version from Cargo.toml
        let updater = AutoUpdater::new(env!("CARGO_PKG_VERSION"));
        cx.set_global(updater.clone());
        updater.start_polling(cx);
        info!("Auto-updater initialized and polling started");

        // Use Arc<AtomicBool> to track when both providers and models are loaded

        let providers_loaded = Arc::new(AtomicBool::new(false));
        let models_loaded = Arc::new(AtomicBool::new(false));

        // Helper function to check if both are loaded and trigger conversation loading
        let check_and_load_conversations = {
            let providers_loaded = providers_loaded.clone();
            let models_loaded = models_loaded.clone();
            move |cx: &mut App| {
                if providers_loaded.load(Ordering::SeqCst) && models_loaded.load(Ordering::SeqCst) {
                    info!("Both models and providers loaded, triggering conversation load");

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
                        check_fn_1(cx);
                    })
                    .ok();
                }
                Err(e) => {
                    error!(error = ?e, "Failed to load providers");
                }
            }
        })
        .detach();

        // Load models asynchronously without blocking startup
        let check_fn_2 = check_and_load_conversations;
        let models_loaded_clone = models_loaded.clone();
        cx.spawn(async move |cx: &mut AsyncApp| {
            let repo = MODELS_REPOSITORY.clone();
            match repo.load_all().await {
                Ok(models) => {
                    cx.update(|cx| {
                        cx.update_global::<settings::models::ModelsModel, _>(|model, _cx| {
                            model.replace_all(models);
                        });

                        models_loaded_clone.store(true, Ordering::SeqCst);
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
                                .ok();
                        })
                        .detach();

                        // Refresh all chat inputs with newly loaded models
                        cx.refresh_windows();

                        check_fn_2(cx);
                    })
                    .ok();
                }
                Err(e) => {
                    error!(error = ?e, "Failed to load models");
                }
            }
        })
        .detach();

        // register actions
        register_actions(cx);

        // Get platform-specific window options for main window
        let options = settings::utils::window_utils::get_main_window_options();

        cx.open_window(options, |window, cx| {
            let view = cx.new(|cx| ChattyApp::new(window, cx));

            cx.new(|cx| Root::new(view, window, cx))
        })
        .expect("Failed to open main window");
    });
}
