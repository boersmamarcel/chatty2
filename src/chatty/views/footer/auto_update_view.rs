use crate::assets::CustomIcon;
use crate::auto_updater::{AutoUpdateStatus, AutoUpdater};
use gpui::*;
use gpui_component::{ActiveTheme as _, Icon, Sizable, button::*, h_flex};

#[derive(IntoElement)]
pub struct AutoUpdateView {
    on_click: Option<Box<dyn Fn(&mut Window, &mut App) + 'static>>,
}

impl AutoUpdateView {
    pub fn new() -> Self {
        Self { on_click: None }
    }

    pub fn on_click<F>(mut self, handler: F) -> Self
    where
        F: Fn(&mut Window, &mut App) + 'static,
    {
        self.on_click = Some(Box::new(handler));
        self
    }
}

impl RenderOnce for AutoUpdateView {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let updater = cx.global::<AutoUpdater>();
        let status = updater.status();
        let version = updater.current_version();

        let (icon, text, tooltip, enabled, highlighted) = match status {
            AutoUpdateStatus::Idle => (
                CustomIcon::Refresh,
                format!("v{}", version),
                "Check for updates".to_string(),
                true,
                false,
            ),
            AutoUpdateStatus::Checking => (
                CustomIcon::Loader,
                "Checking...".to_string(),
                "Checking for updates".to_string(),
                false,
                false,
            ),
            AutoUpdateStatus::Downloading(progress) => (
                CustomIcon::Download,
                format!("{:.0}%", progress * 100.0),
                "Downloading update".to_string(),
                false,
                false,
            ),
            AutoUpdateStatus::Installing => (
                CustomIcon::Loader,
                "Installing...".to_string(),
                "Installing update".to_string(),
                false,
                false,
            ),
            AutoUpdateStatus::Updated { version, .. } => (
                CustomIcon::CheckCircle,
                "Restart to update".to_string(),
                format!("Click to restart and install v{}", version),
                true,
                true, // Highlight this state
            ),
            AutoUpdateStatus::Errored(msg) => (
                CustomIcon::AlertCircle,
                "Update failed".to_string(),
                msg.clone(),
                true,
                false,
            ),
        };

        let mut button = Button::new("auto-update-button")
            .ghost()
            .xsmall()
            .tooltip(tooltip)
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(Icon::new(icon).size(px(16.0)))
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().foreground)
                            .child(text),
                    ),
            );

        // Add highlighted styling for update ready state
        if highlighted {
            button = button.primary();
        }

        // Only enable click handler if the button should be clickable
        if enabled {
            if let Some(handler) = self.on_click {
                button = button.on_click(move |_event, window, cx| {
                    handler(window, cx);
                });
            }
        }

        button
    }
}
