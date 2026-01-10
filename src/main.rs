use gpui::*;
use gpui_component::*;

mod controllers;
mod views;

use controllers::ChattyApp;

fn main() {
    let app = Application::new();

    app.run(move |cx| {
        cx.activate(true);

        // Initialize the theme
        init(cx);

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
