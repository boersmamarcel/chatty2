use crate::settings::controllers::mcp_controller;
use crate::settings::models::mcp_store::McpServersModel;
use gpui::prelude::FluentBuilder;
use gpui_component::button::*;
use gpui_component::setting::{SettingField, SettingGroup, SettingItem, SettingPage};

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

pub fn mcp_tools_page() -> SettingPage {
    SettingPage::new("Tools")
        .resettable(false)
        .groups(vec![mcp_info_group(), mcp_servers_list_group()])
}

fn mcp_info_group() -> SettingGroup {
    SettingGroup::new()
        .title("Model Context Protocol (MCP)")
        .description("MCP servers extend the AI with external tools like file access, web search, databases, and more")
        .items(vec![])
}

fn mcp_servers_list_group() -> SettingGroup {
    SettingGroup::new()
        .title("Configured Servers")
        .description("MCP servers provide tools that the AI can use during conversations")
        .items(vec![
            SettingItem::new(
                "Active Servers",
                SettingField::render(move |_options, _window, cx| {
                    let servers = cx.global::<McpServersModel>().servers().to_vec();

                    use gpui::*;
                    use gpui_component::*;

                    if servers.is_empty() {
                        div()
                            .p_4()
                            .text_color(cx.theme().muted_foreground)
                            .text_sm()
                            .child("No MCP servers configured.")
                            .into_any_element()
                    } else {
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .children(servers.iter().map(|server| {
                                let enabled = server.enabled;
                                let name = server.name.clone();
                                let button_id =
                                    SharedString::from(format!("settings-toggle-{}", name));

                                div()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .p_3()
                                    .rounded_md()
                                    .border_1()
                                    .border_color(cx.theme().border)
                                    .bg(cx.theme().muted)
                                    .child(
                                        div()
                                            .flex()
                                            .flex_col()
                                            .gap_1()
                                            .child(
                                                div()
                                                    .text_sm()
                                                    .font_weight(FontWeight::SEMIBOLD)
                                                    .text_color(cx.theme().foreground)
                                                    .child(server.name.clone()),
                                            )
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(cx.theme().muted_foreground)
                                                    .child(format!(
                                                        "{} {}",
                                                        server.command,
                                                        server.args.join(" ")
                                                    )),
                                            ),
                                    )
                                    .child(
                                        Button::new(button_id)
                                            .xsmall()
                                            .when(enabled, |btn| btn.primary())
                                            .when(!enabled, |btn| btn.ghost())
                                            .child(if enabled { "Enabled" } else { "Disabled" })
                                            .on_click(move |_event, _window, cx| {
                                                mcp_controller::toggle_server(name.clone(), cx);
                                            }),
                                    )
                            }))
                            .into_any_element()
                    }
                }),
            )
            .description(format!(
                "Configure servers by editing: {}",
                get_config_path_display()
            )),
        ])
}
