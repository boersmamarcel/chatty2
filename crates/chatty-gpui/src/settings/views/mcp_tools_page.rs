use crate::settings::controllers::mcp_controller;
use crate::settings::models::mcp_store::McpServersModel;
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::*;
use gpui_component::setting::{SettingGroup, SettingItem, SettingPage};
use gpui_component::{ActiveTheme, Icon, IconName, Sizable, h_flex, v_flex};

pub fn mcp_tools_page() -> SettingPage {
    SettingPage::new("Tools")
        .description(
            "MCP servers extend the AI with external tools like file access, \
             web search, databases, and more. Start the server yourself, then add \
             its URL here.",
        )
        .resettable(false)
        .groups(vec![mcp_servers_list_group()])
}

fn mcp_servers_list_group() -> SettingGroup {
    SettingGroup::new()
        .title("Configured Servers")
        .description(
            "Connect to already-running MCP servers by URL. The server must be \
             running before you enable it here.",
        )
        .items(vec![SettingItem::render(|_options, _window, cx| {
            let servers = cx.global::<McpServersModel>().servers().to_vec();

            if servers.is_empty() {
                v_flex()
                    .w_full()
                    .py_6()
                    .items_center()
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("No MCP servers configured."),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .pt_1()
                            .child(format!(
                                "Add servers by editing: {}",
                                get_config_path_display()
                            )),
                    )
                    .into_any_element()
            } else {
                v_flex()
                    .w_full()
                    .gap_0()
                    .rounded_md()
                    .border_1()
                    .border_color(cx.theme().border)
                    .overflow_hidden()
                    // Header row
                    .child(render_header(cx))
                    // Server rows
                    .children(servers.iter().enumerate().map(|(ix, server)| {
                        render_server_row(
                            ix,
                            server.name.clone(),
                            server.url.clone(),
                            server.has_api_key(),
                            server.enabled,
                            cx,
                        )
                        .into_any_element()
                    }))
                    .into_any_element()
            }
        })])
}

/// Render the table header row.
fn render_header(cx: &App) -> impl IntoElement {
    h_flex()
        .w_full()
        .px_3()
        .py_2()
        .border_b_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().muted)
        .child(
            div()
                .w(px(140.))
                .flex_shrink_0()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(cx.theme().muted_foreground)
                .child("Name"),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(cx.theme().muted_foreground)
                .child("URL"),
        )
        .child(
            div()
                .w(px(120.))
                .flex_shrink_0()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(cx.theme().muted_foreground)
                .text_right()
                .child("Actions"),
        )
}

/// Render a single server row.
fn render_server_row(
    ix: usize,
    name: String,
    url: String,
    has_api_key: bool,
    enabled: bool,
    cx: &App,
) -> impl IntoElement {
    let name_for_toggle = name.clone();
    let name_for_delete = name.clone();
    let toggle_id = SharedString::from(format!("mcp-toggle-{}", ix));
    let delete_id = SharedString::from(format!("mcp-delete-{}", ix));

    h_flex()
        .w_full()
        .px_3()
        .py_2()
        .items_center()
        .border_b_1()
        .border_color(cx.theme().border)
        // Name column — fixed width, no wrap
        .child(
            div()
                .w(px(140.))
                .flex_shrink_0()
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .text_color(cx.theme().foreground)
                .overflow_hidden()
                .whitespace_nowrap()
                .child(name),
        )
        // URL column — flexible, truncates on overflow; shows key badge if authenticated
        .child(
            h_flex()
                .flex_1()
                .min_w_0()
                .gap_1()
                .items_center()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .child(url),
                )
                .when(has_api_key, |this| {
                    this.child(
                        div()
                            .flex_shrink_0()
                            .text_xs()
                            .px_1()
                            .rounded_sm()
                            .bg(cx.theme().muted)
                            .text_color(cx.theme().muted_foreground)
                            .child("🔑"),
                    )
                }),
        )
        // Actions column — fixed width, no shrink
        .child(
            h_flex()
                .w(px(120.))
                .flex_shrink_0()
                .gap_1()
                .justify_end()
                .child(
                    Button::new(toggle_id)
                        .xsmall()
                        .when(enabled, |btn| btn.primary())
                        .when(!enabled, |btn| btn.ghost())
                        .child(if enabled { "Enabled" } else { "Disabled" })
                        .on_click(move |_event, _window, cx| {
                            mcp_controller::toggle_server(name_for_toggle.clone(), cx);
                        }),
                )
                .child(
                    Button::new(delete_id)
                        .icon(Icon::new(IconName::Close))
                        .ghost()
                        .xsmall()
                        .on_click(move |_event, _window, cx| {
                            mcp_controller::delete_server(name_for_delete.clone(), cx);
                        }),
                ),
        )
}

fn get_config_path_display() -> String {
    dirs::config_dir()
        .map(|p| {
            p.join("chatty")
                .join("mcp_servers.json")
                .to_string_lossy()
                .into_owned()
        })
        .unwrap_or_else(|| "chatty/mcp_servers.json".to_string())
}
