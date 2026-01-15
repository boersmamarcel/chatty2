use gpui::*;
use gpui_component::*;

mod chatty;
mod settings;

use chatty::ChattyApp;
use settings::SettingsView;
use settings::repositories::{JsonFileRepository, ProviderRepository};
use std::sync::Arc;

actions!(chatty, [OpenSettings]);

// Global repository
lazy_static::lazy_static! {
    static ref PROVIDER_REPOSITORY: Arc<dyn ProviderRepository> = {
        let repo = JsonFileRepository::new()
            .expect("Failed to initialize provider repository");
        Arc::new(repo)
    };
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

        // Initialize general settings
        cx.set_global(settings::models::general_model::GeneralSettingsModel::default());

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
