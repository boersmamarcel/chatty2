use crate::assets::CustomIcon;
use crate::settings::controllers::execution_settings_controller;
use crate::settings::models::execution_settings::ExecutionSettingsModel;
use gpui::*;
use gpui_component::Icon;
use gpui_component::tooltip::Tooltip;

#[derive(IntoElement, Default)]
pub struct FetchIndicatorView;

impl FetchIndicatorView {
    pub fn new() -> Self {
        Self
    }
}

impl RenderOnce for FetchIndicatorView {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let fetch_enabled = cx.global::<ExecutionSettingsModel>().fetch_enabled;
        // Blue = fetch enabled (online), Red = fetch disabled (offline)
        let icon_color = if fetch_enabled {
            rgb(0x3B82F6) // Blue-500
        } else {
            rgb(0xEF4444) // Red-500
        };
        let tooltip = if fetch_enabled {
            "Online: AI can browse the web and download files (click to go offline)"
        } else {
            "Offline: AI has no internet access (click to go online)"
        };

        div()
            .id("fetch-toggle")
            .cursor_pointer()
            .px_1()
            .py_0p5()
            .child(
                Icon::new(CustomIcon::Earth)
                    .size(px(12.0))
                    .text_color(icon_color),
            )
            .tooltip(move |window, cx| Tooltip::new(tooltip).build(window, cx))
            .on_click(move |_event, _window, cx| {
                execution_settings_controller::toggle_fetch(cx);
            })
    }
}
