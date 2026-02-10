use crate::settings::models::mcp_store::McpServersModel;
use gpui_component::setting::{SettingField, SettingGroup, SettingItem, SettingPage};

/// Get platform-specific config path for display
fn get_config_path_display() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "~/Library/Application Support/chatty/mcp_servers.json"
    }
    #[cfg(target_os = "linux")]
    {
        "~/.config/chatty/mcp_servers.json (or $XDG_CONFIG_HOME/chatty/mcp_servers.json)"
    }
    #[cfg(target_os = "windows")]
    {
        "%APPDATA%\\chatty\\mcp_servers.json"
    }
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
    let config_path = get_config_path_display();

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
                        let empty_message = format!("No MCP servers configured. Add a server by editing {}", config_path);
                        div()
                            .p_4()
                            .text_color(cx.theme().muted_foreground)
                            .text_sm()
                            .child(empty_message)
                            .into_any_element()
                    } else {
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .children(servers.iter().map(|server| {
                                let status_color = if server.enabled {
                                    cx.theme().accent
                                } else {
                                    cx.theme().muted_foreground
                                };

                                let status_text = if server.enabled { "Enabled" } else { "Disabled" };

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
                                                    .child(server.name.clone())
                                            )
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(cx.theme().muted_foreground)
                                                    .child(format!("{} {}", server.command, server.args.join(" ")))
                                            )
                                    )
                                    .child(
                                        div()
                                            .px_2()
                                            .py_1()
                                            .rounded_sm()
                                            .text_xs()
                                            .text_color(status_color)
                                            .border_1()
                                            .border_color(status_color)
                                            .child(status_text)
                                    )
                            }))
                            .into_any_element()
                    }
                }),
            )
            .description("Configure servers by editing the config file at the platform-specific location shown above"),
        ])
}
