use crate::assets::CustomIcon;
use crate::settings::controllers::execution_settings_controller;
use crate::settings::models::execution_settings::ExecutionSettingsModel;
use gpui::*;
use gpui_component::{Icon, Sizable, button::*};

#[derive(IntoElement, Default)]
pub struct NetworkIndicatorView;

impl NetworkIndicatorView {
    pub fn new() -> Self {
        Self
    }
}

impl RenderOnce for NetworkIndicatorView {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let network_isolation = cx.global::<ExecutionSettingsModel>().network_isolation;
        // Blue = network allowed (isolation OFF), Red = network blocked (isolation ON)
        let icon_color = if network_isolation {
            rgb(0xEF4444) // Red-500
        } else {
            rgb(0x3B82F6) // Blue-500
        };
        let tooltip = if network_isolation {
            "Sandbox: network blocked for shell commands (click to allow)"
        } else {
            "Sandbox: network allowed for shell commands (click to block)"
        };

        Button::new("network-isolation-toggle")
            .ghost()
            .xsmall()
            .tooltip(tooltip)
            .child(
                Icon::new(CustomIcon::Codesandbox)
                    .size(px(12.0))
                    .text_color(icon_color),
            )
            .on_click(move |_event, _window, cx| {
                execution_settings_controller::toggle_network_isolation(cx);
            })
    }
}
