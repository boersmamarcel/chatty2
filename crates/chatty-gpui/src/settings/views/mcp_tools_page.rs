use crate::settings::controllers::mcp_controller;
use crate::settings::models::mcp_store::{McpAuthStatus, McpServersModel};
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::*;
use gpui_component::input::{Input, InputState};
use gpui_component::setting::{SettingGroup, SettingItem, SettingPage};
use gpui_component::{ActiveTheme, Icon, IconName, Sizable, WindowExt as _, h_flex, v_flex};

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
            let model = cx.global::<McpServersModel>();
            let servers = model.servers().to_vec();
            let auth_statuses: Vec<McpAuthStatus> = servers
                .iter()
                .map(|s| model.auth_status(&s.name).clone())
                .collect();

            v_flex()
                .w_full()
                .gap_3()
                .when(servers.is_empty(), |this| {
                    this.child(
                        v_flex()
                            .w_full()
                            .py_6()
                            .items_center()
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("No MCP servers configured yet."),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .pt_1()
                                    .child(
                                        "Click \u{201c}Add Server\u{201d} below to get started.",
                                    ),
                            ),
                    )
                })
                .when(!servers.is_empty(), |this| {
                    this.child(
                        v_flex()
                            .w_full()
                            .gap_0()
                            .rounded_md()
                            .border_1()
                            .border_color(cx.theme().border)
                            .overflow_hidden()
                            .child(render_header(cx))
                            .children(servers.iter().enumerate().map(|(ix, server)| {
                                render_server_row(
                                    ix,
                                    server.name.clone(),
                                    server.url.clone(),
                                    server.has_api_key(),
                                    server.enabled,
                                    auth_statuses[ix].clone(),
                                    cx,
                                )
                                .into_any_element()
                            })),
                    )
                })
                .child(
                    h_flex().w_full().justify_start().child(
                        Button::new("add-mcp-server")
                            .label("Add Server")
                            .icon(Icon::new(IconName::Plus))
                            .on_click(|_event, window, cx| {
                                show_add_server_dialog(window, cx);
                            }),
                    ),
                )
                .into_any_element()
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
                .w(px(150.))
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
    auth_status: McpAuthStatus,
    cx: &App,
) -> impl IntoElement {
    let name_for_toggle = name.clone();
    let name_for_delete = name.clone();
    let name_for_key = name.clone();
    let toggle_id = SharedString::from(format!("mcp-toggle-{}", ix));
    let key_id = SharedString::from(format!("mcp-key-{}", ix));
    let delete_id = SharedString::from(format!("mcp-delete-{}", ix));

    // Auth status indicator
    let (status_dot_color, status_text) = match &auth_status {
        McpAuthStatus::NotRequired => (None, None),
        McpAuthStatus::Authenticated => (Some(gpui::green()), Some("Connected")),
        McpAuthStatus::NeedsAuth => (Some(gpui::yellow()), Some("Needs auth")),
        McpAuthStatus::Connecting => (Some(gpui::yellow()), Some("Connecting…")),
        McpAuthStatus::Failed(_) => (Some(gpui::red()), Some("Failed")),
    };

    h_flex()
        .w_full()
        .px_3()
        .py_2()
        .items_center()
        .border_b_1()
        .border_color(cx.theme().border)
        // Name column with optional status dot
        .child(
            h_flex()
                .w(px(140.))
                .flex_shrink_0()
                .gap_1p5()
                .items_center()
                .when_some(status_dot_color, |this, color| {
                    this.child(
                        div()
                            .w(px(8.))
                            .h(px(8.))
                            .rounded_full()
                            .bg(color)
                            .flex_shrink_0(),
                    )
                })
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(cx.theme().foreground)
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .child(name.clone()),
                        )
                        .when_some(status_text, |this, text| {
                            this.child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(text),
                            )
                        }),
                ),
        )
        // URL column
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
        // Actions column
        .child(
            h_flex()
                .w(px(150.))
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
                    Button::new(key_id)
                        .icon(Icon::default().path("icons/key.svg"))
                        .ghost()
                        .xsmall()
                        .tooltip("Edit API Key")
                        .on_click(move |_event, window, cx| {
                            show_edit_key_dialog(name_for_key.clone(), has_api_key, window, cx);
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

/// Open dialog to add a new MCP server.
fn show_add_server_dialog(window: &mut Window, cx: &mut App) {
    let name_input = cx.new(|cx| InputState::new(window, cx).placeholder("e.g. figma"));
    let url_input =
        cx.new(|cx| InputState::new(window, cx).placeholder("http://localhost:3000/mcp"));
    let api_key_input = cx.new(|cx| {
        InputState::new(window, cx)
            .masked(true)
            .placeholder("Optional bearer token")
    });

    window.open_dialog(cx, move |dialog, _window, _cx| {
        dialog
            .title("Add MCP Server")
            .overlay(true)
            .keyboard(true)
            .close_button(true)
            .overlay_closable(true)
            .w(px(500.))
            .child(
                div().id("add-mcp-server-form").child(
                    v_flex()
                        .gap_3()
                        .p_4()
                        .child(
                            v_flex()
                                .gap_1()
                                .child(div().text_sm().child("Server Name"))
                                .child(Input::new(&name_input)),
                        )
                        .child(
                            v_flex()
                                .gap_1()
                                .child(div().text_sm().child("Server URL"))
                                .child(Input::new(&url_input)),
                        )
                        .child(
                            v_flex()
                                .gap_1()
                                .child(div().text_sm().child("API Key (optional)"))
                                .child(Input::new(&api_key_input).mask_toggle()),
                        )
                        .child(
                            h_flex()
                                .gap_2()
                                .justify_end()
                                .pt_4()
                                .child(Button::new("cancel-add-mcp").label("Cancel").on_click(
                                    move |_, window, cx| {
                                        window.close_dialog(cx);
                                    },
                                ))
                                .child(
                                    Button::new("save-add-mcp")
                                        .primary()
                                        .label("Add")
                                        .on_click({
                                            let name_input = name_input.clone();
                                            let url_input = url_input.clone();
                                            let api_key_input = api_key_input.clone();
                                            move |_, window, cx| {
                                                let name =
                                                    name_input.read(cx).value().trim().to_string();
                                                let url =
                                                    url_input.read(cx).value().trim().to_string();
                                                let api_key = api_key_input
                                                    .read(cx)
                                                    .value()
                                                    .trim()
                                                    .to_string();

                                                if name.is_empty() {
                                                    window.push_notification(
                                                        "Server name is required",
                                                        cx,
                                                    );
                                                    return;
                                                }

                                                if url.is_empty() {
                                                    window.push_notification(
                                                        "Server URL is required",
                                                        cx,
                                                    );
                                                    return;
                                                }

                                                if !url.starts_with("http://")
                                                    && !url.starts_with("https://")
                                                {
                                                    window.push_notification(
                                                        "URL must start with http:// or https://",
                                                        cx,
                                                    );
                                                    return;
                                                }

                                                let exists = cx
                                                    .global::<McpServersModel>()
                                                    .servers()
                                                    .iter()
                                                    .any(|s| s.name == name);
                                                if exists {
                                                    let msg =
                                                        format!("Server '{}' already exists", name);
                                                    window.push_notification(msg, cx);
                                                    return;
                                                }

                                                let api_key = if api_key.is_empty() {
                                                    None
                                                } else {
                                                    Some(api_key)
                                                };

                                                mcp_controller::create_server(
                                                    name, url, api_key, cx,
                                                );
                                                window.close_dialog(cx);
                                            }
                                        }),
                                ),
                        ),
                ),
            )
    });
}

/// Open dialog to edit the API key for an existing server.
fn show_edit_key_dialog(
    server_name: String,
    has_existing_key: bool,
    window: &mut Window,
    cx: &mut App,
) {
    let placeholder = if has_existing_key {
        "Enter new key or leave empty to remove"
    } else {
        "Bearer token"
    };
    let api_key_input = cx.new(|cx| {
        InputState::new(window, cx)
            .masked(true)
            .placeholder(placeholder)
    });
    let title: SharedString = format!("API Key \u{2014} {}", server_name).into();

    window.open_dialog(cx, move |dialog, _window, _cx| {
        dialog
            .title(title.clone())
            .overlay(true)
            .keyboard(true)
            .close_button(true)
            .overlay_closable(true)
            .w(px(450.))
            .child(
                div().id("edit-mcp-key-form").child(
                    v_flex()
                        .gap_3()
                        .p_4()
                        .child(
                            v_flex()
                                .gap_1()
                                .child(div().text_sm().child("API Key"))
                                .child(Input::new(&api_key_input).mask_toggle()),
                        )
                        .child(
                            h_flex()
                                .gap_2()
                                .justify_end()
                                .pt_4()
                                .child(Button::new("cancel-edit-key").label("Cancel").on_click(
                                    move |_, window, cx| {
                                        window.close_dialog(cx);
                                    },
                                ))
                                .child(
                                    Button::new("save-edit-key")
                                        .primary()
                                        .label("Save")
                                        .on_click({
                                            let api_key_input = api_key_input.clone();
                                            let server_name = server_name.clone();
                                            move |_, window, cx| {
                                                let api_key = api_key_input
                                                    .read(cx)
                                                    .value()
                                                    .trim()
                                                    .to_string();
                                                let api_key = if api_key.is_empty() {
                                                    None
                                                } else {
                                                    Some(api_key)
                                                };

                                                mcp_controller::update_server_api_key(
                                                    server_name.clone(),
                                                    api_key,
                                                    cx,
                                                );
                                                window.close_dialog(cx);
                                            }
                                        }),
                                ),
                        ),
                ),
            )
    });
}
