use crate::assets::CustomIcon;
use crate::settings::controllers::mcp_controller;
use crate::settings::models::mcp_store::{McpServerConfig, McpServersModel};
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::popover::Popover;
use gpui_component::{ActiveTheme, Icon, Sizable, button::*, h_flex};

#[derive(IntoElement)]
pub struct McpIndicatorView;

impl McpIndicatorView {
    pub fn new() -> Self {
        Self
    }
}

impl RenderOnce for McpIndicatorView {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let store = cx.global::<McpServersModel>();
        let enabled_count = store.enabled_count();
        let all_servers = store.servers().to_vec();
        let total_count = all_servers.len();

        // MCP blue color (matches brand)
        let mcp_color = rgb(0x3B82F6); // Blue-500

        div().when(total_count > 0, |this| {
            // Main indicator button
            let indicator_button = Button::new("mcp-indicator")
                .ghost()
                .xsmall()
                .tooltip(format!(
                    "{} MCP server{} enabled",
                    enabled_count,
                    if enabled_count == 1 { "" } else { "s" }
                ))
                .child(
                    h_flex()
                        .gap_1()
                        .items_center()
                        .child(
                            Icon::new(CustomIcon::McpServer)
                                .size(px(12.0))
                                .text_color(mcp_color),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(mcp_color)
                                .child(enabled_count.to_string()),
                        ),
                );

            // Popover with server list
            this.child(
                Popover::new("mcp-server-list")
                    .trigger(indicator_button)
                    .appearance(false)
                    .content(move |_, _window, cx| {
                        let servers = all_servers.clone();

                        div()
                            .flex()
                            .flex_col()
                            .bg(cx.theme().background)
                            .border_1()
                            .border_color(cx.theme().border)
                            .rounded_md()
                            .shadow_md()
                            .p_2()
                            .min_w(px(200.0))
                            .max_w(px(300.0))
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(cx.theme().foreground)
                                    .pb_2()
                                    .child("MCP Servers"),
                            )
                            .child(div().h(px(1.0)).w_full().bg(cx.theme().border).mb_2())
                            .children(
                                servers
                                    .into_iter()
                                    .map(|server| render_server_item(server))
                                    .collect::<Vec<_>>(),
                            )
                    }),
            )
        })
    }
}

/// Render a single server item in the popover
fn render_server_item(server: McpServerConfig) -> impl IntoElement {
    let name = server.name.clone();
    let enabled = server.enabled;
    let name_for_click = name.clone();
    let button_id = SharedString::from(format!("toggle-{}", name_for_click));

    div()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .gap_2()
        .px_2()
        .py_1()
        .rounded_md()
        .child(div().text_sm().child(name))
        .child(
            Button::new(button_id)
                .xsmall()
                .when(enabled, |btn| btn.primary())
                .when(!enabled, |btn| btn.ghost())
                .child(if enabled { "Enabled" } else { "Disabled" })
                .on_click(move |_event, _window, cx| {
                    mcp_controller::toggle_server(name_for_click.clone(), cx);
                }),
        )
}
