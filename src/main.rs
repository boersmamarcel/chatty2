use gpui::*;
use gpui_component::*;

mod chatty;
mod settings;

use chatty::ChattyApp;
use settings::SettingsView;
use settings::repositories::{
    GeneralSettingsJsonRepository, GeneralSettingsRepository, JsonFileRepository,
    ProviderRepository,
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
}

fn init_themes(cx: &mut App) {
    // Load saved base theme name from GeneralSettingsModel
    let saved_base_theme = cx
        .global::<settings::models::general_model::GeneralSettingsModel>()
        .theme_name
        .clone()
        .unwrap_or_else(|| "Ayu".to_string());

    // Watch themes directory and apply saved theme when loaded
    if let Err(err) = ThemeRegistry::watch_dir(PathBuf::from("./themes"), cx, move |cx| {
        // Find the appropriate variant based on current mode
        let is_dark = cx.theme().mode.is_dark();
        let full_theme_name = find_theme_variant_for_init(cx, &saved_base_theme, is_dark);

        if let Some(theme) = ThemeRegistry::global(cx)
            .themes()
            .get(&full_theme_name)
            .cloned()
        {
            Theme::global_mut(cx).apply_config(&theme);
        }
    }) {
        eprintln!("Failed to watch themes directory: {}", err);
    }

    // Observe theme changes and persist base theme name to GeneralSettingsModel
    cx.observe_global::<Theme>(|cx| {
        let full_theme_name = cx.theme().theme_name().to_string();

        // Extract base theme name (remove " Light" or " Dark" suffix)
        let base_theme_name = if full_theme_name.ends_with(" Light") {
            full_theme_name.strip_suffix(" Light").unwrap().to_string()
        } else if full_theme_name.ends_with(" Dark") {
            full_theme_name.strip_suffix(" Dark").unwrap().to_string()
        } else {
            full_theme_name
        };

        // Update model with base name
        cx.global_mut::<settings::models::general_model::GeneralSettingsModel>()
            .theme_name = Some(base_theme_name);

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

/// Helper function to find theme variant during initialization
fn find_theme_variant_for_init(cx: &App, base_name: &str, is_dark: bool) -> SharedString {
    let all_themes = ThemeRegistry::global(cx).themes();

    let candidates = if is_dark {
        vec![format!("{} Dark", base_name), base_name.to_string()]
    } else {
        vec![format!("{} Light", base_name), base_name.to_string()]
    };

    for candidate in candidates {
        let candidate_shared: SharedString = candidate.into();
        if all_themes.contains_key(&candidate_shared) {
            return candidate_shared;
        }
    }

    base_name.to_string().into()
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

fn main() {
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
                        cx.set_global(settings);
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

        // Initialize global settings window state
        cx.set_global(settings::controllers::GlobalSettingsWindow::default());

        // Load providers asynchronously without blocking startup
        cx.spawn(async move |cx: &mut AsyncApp| {
            let repo = PROVIDER_REPOSITORY.clone();
            match repo.load_all().await {
                Ok(providers) => {
                    cx.update(|cx| {
                        cx.update_global::<settings::models::ProviderModel, _>(|model, _cx| {
                            model.replace_all(providers);
                        });
                    })
                    .ok();
                }
                Err(e) => {
                    eprintln!("Failed to load providers: {}", e);
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
