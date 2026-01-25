use gpui::*;
use gpui_component::*;

mod chatty;
mod settings;

use chatty::{ChattyApp, GlobalChattyApp};
use settings::SettingsView;
use settings::repositories::{
    GeneralSettingsJsonRepository, GeneralSettingsRepository, JsonFileRepository,
    JsonModelsRepository, ModelsRepository, ProviderRepository,
};
use std::path::PathBuf;
use std::sync::Arc;

actions!(chatty, [OpenSettings]);

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
        eprintln!("Failed to watch themes directory: {}", err);
    }

    // Observe theme changes and persist base theme name + dark mode to GeneralSettingsModel
    // This will only trigger when user makes changes, not on initial load
    cx.observe_global::<Theme>(|cx| {
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
                eprintln!("Failed to save theme preference: {}", e);
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
    }
}

fn register_actions(cx: &mut App) {
    // Register open settings action
    println!("Action registered");
    cx.bind_keys([KeyBinding::new("cmd-,", OpenSettings, None)]);
    cx.on_action(|_: &OpenSettings, cx: &mut App| {
        println!("Action triggered");
        SettingsView::open_or_focus_settings_window(cx);
    });
}

/// Discover available Ollama models by querying the Ollama API
async fn discover_ollama_models(base_url: &str) -> anyhow::Result<Vec<(String, String)>> {
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Deserialize, Serialize)]
    struct OllamaModel {
        name: String,
        #[serde(default)]
        model: String,
    }

    #[derive(Debug, Deserialize, Serialize)]
    struct OllamaTagsResponse {
        models: Vec<OllamaModel>,
    }

    // Build the API endpoint URL
    let url = format!("{}/api/tags", base_url.trim_end_matches('/'));

    // Make HTTP request to Ollama API
    let response = reqwest::get(&url).await?;

    if !response.status().is_success() {
        return Err(anyhow::anyhow!(
            "Ollama API returned status: {}",
            response.status()
        ));
    }

    let tags_response: OllamaTagsResponse = response.json().await?;

    // Extract model names and create display names
    let models: Vec<(String, String)> = tags_response
        .models
        .into_iter()
        .map(|m| {
            let identifier = m.name.clone();
            // Create a friendly display name (capitalize first letter, remove tags)
            let display_name = identifier
                .split(':')
                .next()
                .unwrap_or(&identifier)
                .split('-')
                .map(|s| {
                    let mut c = s.chars();
                    match c.next() {
                        None => String::new(),
                        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                    }
                })
                .collect::<Vec<_>>()
                .join(" ");

            (identifier, display_name)
        })
        .collect();

    Ok(models)
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
    // This allows rig to use Tokio even though GPUI uses smol
    let _guard = _tokio_runtime.enter();

    let app = Application::new();

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
                    eprintln!("Failed to load general settings: {}", e);
                    eprintln!("Using default settings");
                    // Already initialized with defaults above, so no action needed
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

        // Use Arc<AtomicBool> to track when both providers and models are loaded
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        let providers_loaded = Arc::new(AtomicBool::new(false));
        let models_loaded = Arc::new(AtomicBool::new(false));

        // Helper function to check if both are loaded and trigger conversation loading
        let check_and_load_conversations = {
            let providers_loaded = providers_loaded.clone();
            let models_loaded = models_loaded.clone();
            move |cx: &mut App| {
                if providers_loaded.load(Ordering::SeqCst) && models_loaded.load(Ordering::SeqCst) {
                    eprintln!(
                        "✅ [main] Both models and providers loaded, triggering conversation load"
                    );

                    // Get the ChattyApp entity and call load_conversations_after_models_ready
                    if let Some(weak_entity) = cx
                        .try_global::<GlobalChattyApp>()
                        .and_then(|global| global.entity.clone())
                    {
                        if let Some(entity) = weak_entity.upgrade() {
                            entity.update(cx, |app, cx| {
                                app.load_conversations_after_models_ready(cx);
                            });
                        }
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
                        let mut should_save = false;

                        cx.update_global::<settings::models::ProviderModel, _>(|model, _cx| {
                            model.replace_all(providers);

                            // Ensure Ollama provider exists with default settings
                            if !model.providers().iter().any(|p| {
                                matches!(p.provider_type, settings::models::providers_store::ProviderType::Ollama)
                            }) {
                                use settings::models::providers_store::{ProviderConfig, ProviderType};
                                let ollama_config =
                                    ProviderConfig::new("Ollama".to_string(), ProviderType::Ollama)
                                        .with_base_url("http://localhost:11434".to_string());
                                model.add_provider(ollama_config);
                                eprintln!("✨ [main] Created default Ollama provider");
                                should_save = true;
                            }
                        });

                        // Save the updated providers if we added Ollama
                        if should_save {
                            let providers_to_save = cx
                                .global::<settings::models::ProviderModel>()
                                .providers()
                                .to_vec();
                            let repo_for_save = repo.clone();
                            cx.spawn(|_cx: &mut AsyncApp| async move {
                                if let Err(e) = repo_for_save.save_all(providers_to_save).await {
                                    eprintln!("Failed to persist default Ollama provider: {}", e);
                                }
                            })
                            .detach();
                        }

                        providers_loaded_clone.store(true, Ordering::SeqCst);
                        eprintln!("✅ [main] Providers loaded");
                        check_fn_1(cx);
                    })
                    .ok();
                }
                Err(e) => {
                    eprintln!("Failed to load providers: {}", e);
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
                        eprintln!("✅ [main] Models loaded");

                        // Always attempt to auto-discover Ollama models on startup
                        eprintln!("🔍 [main] Attempting Ollama model auto-discovery");

                        // Get Ollama base URL
                        let ollama_base_url = cx
                            .global::<settings::models::ProviderModel>()
                            .providers()
                            .iter()
                            .find(|p| matches!(p.provider_type, settings::models::providers_store::ProviderType::Ollama))
                            .and_then(|p| p.base_url.clone())
                            .unwrap_or_else(|| "http://localhost:11434".to_string());

                        let models_repo = repo.clone();
                        cx.spawn(async move |cx: &mut AsyncApp| {
                                match discover_ollama_models(&ollama_base_url).await {
                                    Ok(discovered_models) if !discovered_models.is_empty() => {
                                        eprintln!(
                                            "✨ [main] Discovered {} Ollama model(s)",
                                            discovered_models.len()
                                        );

                                        // Create ModelConfig for each discovered model
                                        let new_model_configs: Vec<settings::models::models_store::ModelConfig> =
                                            discovered_models
                                                .iter()
                                                .map(|(identifier, display_name)| {
                                                    use settings::models::models_store::ModelConfig;
                                                    use settings::models::providers_store::ProviderType;
                                                    let id = format!("ollama-{}", identifier.replace(':', "-"));
                                                    ModelConfig::new(
                                                        id,
                                                        display_name.clone(),
                                                        ProviderType::Ollama,
                                                        identifier.clone(),
                                                    )
                                                })
                                                .collect();

                                        // Sync Ollama models: remove old ones, add new ones
                                        cx.update(|cx| {
                                            cx.update_global::<settings::models::ModelsModel, _>(
                                                |model, _cx| {
                                                    // Get existing Ollama model IDs
                                                    let existing_ollama_ids: Vec<String> = model
                                                        .models_by_provider(&settings::models::providers_store::ProviderType::Ollama)
                                                        .iter()
                                                        .map(|m| m.id.clone())
                                                        .collect();

                                                    // Remove all existing Ollama models
                                                    for id in existing_ollama_ids {
                                                        model.delete_model(&id);
                                                    }

                                                    // Add newly discovered models
                                                    for config in &new_model_configs {
                                                        model.add_model(config.clone());
                                                    }

                                                    eprintln!(
                                                        "�� [main] Synced Ollama models: {} model(s) now available",
                                                        new_model_configs.len()
                                                    );
                                                },
                                            );

                                            // Refresh windows to update UI
                                            cx.refresh_windows();
                                        })
                                        .ok();

                                        // Save to disk
                                        let all_models = cx
                                            .update(|cx| {
                                                cx.global::<settings::models::ModelsModel>()
                                                    .models()
                                                    .to_vec()
                                            })
                                            .ok();

                                        if let Some(all_models) = all_models {
                                            if let Err(e) = models_repo.save_all(all_models).await {
                                                eprintln!(
                                                    "⚠️  [main] Failed to save discovered Ollama models: {}",
                                                    e
                                                );
                                            } else {
                                                eprintln!("💾 [main] Ollama models saved to disk");
                                            }
                                        }
                                    }
                                    Ok(_) => {
                                        eprintln!("ℹ️  [main] No Ollama models installed at {}", ollama_base_url);
                                        eprintln!("   Install models with: ollama pull <model-name>");

                                        // Remove any existing Ollama models since none are available
                                        cx.update(|cx| {
                                            cx.update_global::<settings::models::ModelsModel, _>(
                                                |model, _cx| {
                                                    let existing_ollama_ids: Vec<String> = model
                                                        .models_by_provider(&settings::models::providers_store::ProviderType::Ollama)
                                                        .iter()
                                                        .map(|m| m.id.clone())
                                                        .collect();

                                                    for id in existing_ollama_ids {
                                                        model.delete_model(&id);
                                                    }
                                                },
                                            );
                                        }).ok();
                                    }
                                    Err(e) => {
                                        eprintln!(
                                            "⚠️  [main] Could not connect to Ollama at {}: {}",
                                            ollama_base_url, e
                                        );
                                        eprintln!("   Make sure Ollama is running or install it from: https://ollama.ai");
                                    }
                                }
                            })
                            .detach();

                        // Refresh all chat inputs with newly loaded models
                        cx.refresh_windows();

                        check_fn_2(cx);
                    })
                    .ok();
                }
                Err(e) => {
                    eprintln!("Failed to load models: {}", e);
                }
            }
        })
        .detach();

        // register actions
        register_actions(cx);

        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds {
                origin: Point::default(),
                size: size(px(1000.0), px(600.0)),
            })),
            titlebar: Some(TitlebarOptions {
                title: Some("Chatty".into()),
                ..Default::default()
            }),
            ..Default::default()
        };

        cx.open_window(options, |window, cx| {
            let view = cx.new(|cx| ChattyApp::new(window, cx));

            cx.new(|cx| Root::new(view, window, cx))
        })
        .expect("Failed to open main window");
    });
}
