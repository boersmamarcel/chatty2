use gpui::*;
use gpui_component::*;

mod chatty;
mod settings;

use chatty::ChattyApp;
use settings::SettingsView;

actions!(chatty, [OpenSettings]);

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

        // Initialize providers model
        cx.set_global(settings::models::providers_model::ProviderModel::new());

        // Initialize global settings window state
        cx.set_global(settings::controllers::GlobalSettingsWindow::default());

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
        .unwrap();
    });
}
