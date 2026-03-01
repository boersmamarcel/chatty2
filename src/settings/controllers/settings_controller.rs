use crate::settings::models::{AgentConfigEvent, GlobalAgentConfigNotifier};
use crate::settings::utils::get_all_base_theme_names;
use gpui::*;
use gpui_component::Root;
use tracing::trace;

// Global state to track the settings window handle
#[derive(Default)]
pub struct GlobalSettingsWindow {
    handle: Option<WindowHandle<Root>>,
}

impl Global for GlobalSettingsWindow {}

pub struct SettingsView {
    /// Cached theme options (base theme names) to avoid recomputing on every render
    pub cached_theme_options: Vec<(SharedString, SharedString)>,
}

impl SettingsView {
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Register a callback to clear the global handle when this window is released
        cx.on_release(|_view, cx| {
            trace!("SettingsView released - clearing global handle");
            cx.global_mut::<GlobalSettingsWindow>().handle = None;
        })
        .detach();

        // Subscribe to agent config notifier so the settings page re-renders
        // when MCP servers, secrets, or execution settings change.
        if let Some(weak_notifier) = cx
            .try_global::<GlobalAgentConfigNotifier>()
            .and_then(|g| g.entity.clone())
            && let Some(notifier) = weak_notifier.upgrade()
        {
            cx.subscribe(
                &notifier,
                |_this, _notifier, event: &AgentConfigEvent, cx| {
                    if matches!(event, AgentConfigEvent::RebuildRequired) {
                        cx.notify();
                    }
                },
            )
            .detach();
        }

        // Compute theme options once at initialization
        let cached_theme_options = get_all_base_theme_names(cx);

        Self {
            cached_theme_options,
        }
    }

    pub fn open_or_focus_settings_window(cx: &mut App) {
        // Check if we have a stored window handle
        if let Some(handle) = cx.global::<GlobalSettingsWindow>().handle {
            trace!(
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

        trace!("Creating new settings window");

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
                size: size(px(1050.0), px(700.0)),
            })),
            window_min_size: Some(size(px(850.0), px(500.0))),
            ..Default::default()
        };

        if let Ok(window_handle) = cx.open_window(options, |window, cx| {
            let view = cx.new(|cx| SettingsView::new(window, cx));
            cx.new(|cx| Root::new(view, window, cx))
        }) {
            trace!(
                "Stored window handle (window_id: {:?})",
                window_handle.window_id()
            );
            cx.global_mut::<GlobalSettingsWindow>().handle = Some(window_handle);
        }
    }
}
