use crate::assets::CustomIcon;
use crate::auto_updater::{AutoUpdateStatus, AutoUpdater};
use crate::chatty::views::footer::progress_circle::ProgressCircle;
use gpui::*;
use gpui_component::{ActiveTheme as _, Icon, Sizable, button::*, h_flex, tooltip::Tooltip};

type ClickHandler = Box<dyn Fn(&mut Window, &mut App) + 'static>;

#[derive(IntoElement)]
pub struct AutoUpdateView {
    on_click: Option<ClickHandler>,
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

impl AutoUpdateView {
    fn render_button(
        self,
        icon: CustomIcon,
        text: String,
        tooltip: &str,
        enabled: bool,
    ) -> AnyElement {
        let mut button = Button::new("auto-update-button")
            .ghost()
            .xsmall()
            .tooltip(tooltip.to_string())
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(Icon::new(icon).size(px(12.0)))
                    .child(div().text_xs().child(text)),
            );

        if enabled && let Some(handler) = self.on_click {
            button = button.on_click(move |_event, window, cx| {
                handler(window, cx);
            });
        }

        button.into_any_element()
    }
}

impl RenderOnce for AutoUpdateView {
    #[allow(refining_impl_trait)]
    fn render(self, _window: &mut Window, cx: &mut App) -> AnyElement {
        let updater = cx.global::<AutoUpdater>();
        let status = updater.status();
        let version = updater.current_version();

        match status {
            AutoUpdateStatus::Downloading(progress) => {
                // Show progress circle during download
                let pct = progress * 100.0;
                let tooltip_text: SharedString = format!("Downloading update ({:.0}%)", pct).into();
                div()
                    .id("auto-update-downloading")
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(
                        ProgressCircle::new("auto-update-progress")
                            .value(pct)
                            .small(),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().foreground)
                            .child("Downloading..."),
                    )
                    .tooltip(move |window, cx| Tooltip::new(tooltip_text.clone()).build(window, cx))
                    .into_any_element()
            }
            AutoUpdateStatus::Idle => self.render_button(
                CustomIcon::Refresh,
                format!("v{}", version),
                "Check for updates",
                true,
            ),
            AutoUpdateStatus::Checking => self.render_button(
                CustomIcon::Loader,
                "Checking...".into(),
                "Checking for updates",
                false,
            ),
            AutoUpdateStatus::Ready(version, _) => self.render_button(
                CustomIcon::CheckCircle,
                format!("v{} ready", version),
                &format!("Click to restart and install v{}", version),
                true,
            ),
            AutoUpdateStatus::Installing => self.render_button(
                CustomIcon::Loader,
                "Installing...".into(),
                "Installing update, app will restart shortly",
                false,
            ),
            AutoUpdateStatus::Error(msg) => {
                self.render_button(CustomIcon::AlertCircle, "Update failed".into(), msg, true)
            }
        }
    }
}
