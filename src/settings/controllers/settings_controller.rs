use gpui::*;

// Global state to track the settings window handle
pub struct GlobalSettingsWindow {
    handle: Option<WindowHandle<SettingsView>>,
}

impl Default for GlobalSettingsWindow {
    fn default() -> Self {
        Self { handle: None }
    }
}

impl Global for GlobalSettingsWindow {}

pub struct SettingsView {}

impl SettingsView {
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Register a callback to clear the global handle when this window is released
        cx.on_release(|_view, cx| {
            println!("SettingsView released - clearing global handle");
            cx.global_mut::<GlobalSettingsWindow>().handle = None;
        })
        .detach();

        Self {}
    }

    pub fn open_or_focus_settings_window(cx: &mut App) {
        // Check if we have a stored window handle
        if let Some(handle) = cx.global::<GlobalSettingsWindow>().handle {
            println!(
                "Window handle exists (window_id: {:?}), attempting to activate",
                handle.window_id()
            );

            // Try to activate - if this fails, on_release will clear the handle
            let _ = handle.update(cx, |_view, window, _cx| {
                window.activate_window();
            });

            // Always return here - if window was closed, on_release will handle cleanup
            // and user can press the key again to create a new window
            return;
        }

        println!("Creating new settings window");

        // Create a new settings window
        let options = WindowOptions {
            titlebar: Some(TitlebarOptions {
                title: Some(SharedString::from("Chatty Settings")),
                appears_transparent: false,
                traffic_light_position: None,
            }),
            window_decorations: None,
            window_bounds: Some(WindowBounds::Windowed(Bounds {
                origin: Point::default(),
                size: size(px(700.0), px(600.0)),
            })),
            window_min_size: Some(size(px(500.0), px(400.0))),
            ..Default::default()
        };

        if let Ok(window_handle) = cx.open_window(options, |window, cx| {
            cx.new(|cx| SettingsView::new(window, cx))
        }) {
            println!(
                "Stored window handle (window_id: {:?})",
                window_handle.window_id()
            );
            cx.global_mut::<GlobalSettingsWindow>().handle = Some(window_handle);
        }
    }
}
