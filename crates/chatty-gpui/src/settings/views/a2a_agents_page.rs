use crate::settings::controllers::a2a_controller;
use crate::settings::models::{DiscoveredModuleEntry, DiscoveredModulesModel, ModuleLoadStatus};
use chatty_core::settings::models::a2a_store::{A2aAgentStatus, A2aAgentsModel};
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::*;
use gpui_component::input::{Input, InputState};
use gpui_component::setting::{SettingGroup, SettingItem, SettingPage};
use gpui_component::{
    ActiveTheme, Icon, IconName, Sizable, StyledExt as _, WindowExt as _, h_flex, v_flex,
};

pub fn a2a_agents_page() -> SettingPage {
    SettingPage::new("A2A Agents")
        .description(
            "A2A (Agent-to-Agent) lets chatty dispatch tasks to remote agents over HTTP. \
             Add an agent URL below; chatty will discover its capabilities automatically. \
             Use `/agent <name> <prompt>` to call a remote agent. \
             Local WASM modules with agent capability are listed below for reference.",
        )
        .resettable(false)
        .groups(vec![a2a_agents_list_group(), local_module_agents_group()])
}

fn a2a_agents_list_group() -> SettingGroup {
    SettingGroup::new()
        .title("Configured Agents")
        .description(
            "Remote A2A agents are called with `/agent <name> <prompt>`. \
             Chatty fetches the agent card automatically to discover skills.",
        )
        .items(vec![SettingItem::render(|_options, _window, cx| {
            let model = cx.global::<A2aAgentsModel>();
            let agents = model.agents().to_vec();
            let statuses: Vec<A2aAgentStatus> = agents
                .iter()
                .map(|a| model.status(&a.name).clone())
                .collect();

            v_flex()
                .w_full()
                .gap_3()
                .when(agents.is_empty(), |this| {
                    this.child(
                        v_flex()
                            .w_full()
                            .py_6()
                            .items_center()
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("No A2A agents configured yet."),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .pt_1()
                                    .child("Click \u{201c}Add Agent\u{201d} below to get started."),
                            ),
                    )
                })
                .when(!agents.is_empty(), |this| {
                    this.child(
                        v_flex()
                            .w_full()
                            .gap_0()
                            .rounded_md()
                            .border_1()
                            .border_color(cx.theme().border)
                            .overflow_hidden()
                            .child(render_header(cx))
                            .children(agents.iter().enumerate().map(|(ix, agent)| {
                                render_agent_row(
                                    ix,
                                    agent.name.clone(),
                                    agent.url.clone(),
                                    agent.skills.clone(),
                                    agent.has_api_key(),
                                    agent.enabled,
                                    statuses[ix].clone(),
                                    cx,
                                )
                                .into_any_element()
                            })),
                    )
                })
                .child(
                    h_flex().w_full().justify_start().child(
                        Button::new("add-a2a-agent")
                            .label("Add Agent")
                            .icon(Icon::new(IconName::Plus))
                            .on_click(|_event, window, cx| {
                                show_add_agent_dialog(window, cx);
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
                .child("URL / Skills"),
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

#[allow(clippy::too_many_arguments)]
fn render_agent_row(
    ix: usize,
    name: String,
    url: String,
    skills: Vec<String>,
    has_api_key: bool,
    enabled: bool,
    status: A2aAgentStatus,
    cx: &App,
) -> impl IntoElement {
    let name_for_toggle = name.clone();
    let name_for_delete = name.clone();
    let name_for_key = name.clone();
    let toggle_id = SharedString::from(format!("a2a-toggle-{}", ix));
    let key_id = SharedString::from(format!("a2a-key-{}", ix));
    let delete_id = SharedString::from(format!("a2a-delete-{}", ix));

    let (status_dot_color, status_text) = match &status {
        A2aAgentStatus::Unknown => (None, None),
        A2aAgentStatus::Connected => (Some(gpui::green()), Some("Connected")),
        A2aAgentStatus::Connecting => (Some(gpui::yellow()), Some("Connecting…")),
        A2aAgentStatus::Failed(_) => (Some(gpui::red()), Some("Unreachable")),
    };

    // Skills summary (show first 2 skill names separated by commas)
    let skills_summary = if skills.is_empty() {
        url.clone()
    } else {
        let visible: Vec<&str> = skills.iter().map(String::as_str).take(2).collect();
        let summary = visible.join(", ");
        if skills.len() > 2 {
            format!("{} +{}", summary, skills.len() - 2)
        } else {
            summary
        }
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
        // URL / Skills column
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .overflow_hidden()
                .whitespace_nowrap()
                .child(skills_summary),
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
                            a2a_controller::toggle_agent(name_for_toggle.clone(), cx);
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
                            a2a_controller::delete_agent(name_for_delete.clone(), cx);
                        }),
                ),
        )
}

/// Open dialog to add a new A2A agent.
fn show_add_agent_dialog(window: &mut Window, cx: &mut App) {
    let name_input = cx.new(|cx| InputState::new(window, cx).placeholder("e.g. voucher-agent"));
    let url_input =
        cx.new(|cx| InputState::new(window, cx).placeholder("https://hive.dev/a2a/voucher-agent"));
    let api_key_input = cx.new(|cx| {
        InputState::new(window, cx)
            .masked(true)
            .placeholder("Optional bearer token")
    });
    // Capture color before the move closure borrows `cx`.
    let muted_fg = cx.theme().muted_foreground;

    window.open_dialog(cx, move |dialog, _window, _cx| {
        dialog
            .title("Add A2A Agent")
            .overlay(true)
            .keyboard(true)
            .close_button(true)
            .overlay_closable(true)
            .w(px(500.))
            .child(
                div().id("add-a2a-agent-form").child(
                    v_flex()
                        .gap_3()
                        .p_4()
                        .child(
                            v_flex()
                                .gap_1()
                                .child(div().text_sm().child("Agent Name"))
                                .child(Input::new(&name_input)),
                        )
                        .child(
                            v_flex()
                                .gap_1()
                                .child(div().text_sm().child("Agent URL"))
                                .child(Input::new(&url_input)),
                        )
                        .child(
                            v_flex()
                                .gap_1()
                                .child(div().text_sm().child("API Key (optional)"))
                                .child(Input::new(&api_key_input).mask_toggle()),
                        )
                        .child(div().text_xs().text_color(muted_fg).child(
                            "Chatty will fetch the agent card automatically. \
                                     Call this agent with `/agent <name> <prompt>`.",
                        ))
                        .child(
                            h_flex()
                                .gap_2()
                                .justify_end()
                                .pt_4()
                                .child(Button::new("cancel-add-a2a").label("Cancel").on_click(
                                    move |_, window, cx| {
                                        window.close_dialog(cx);
                                    },
                                ))
                                .child(
                                    Button::new("save-add-a2a")
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
                                                        "Agent name is required",
                                                        cx,
                                                    );
                                                    return;
                                                }

                                                // Names must be single words (no spaces) so they
                                                // work as the first token of `/agent <name> <prompt>`.
                                                if name.contains(char::is_whitespace) {
                                                    window.push_notification(
                                                        "Agent name must not contain spaces",
                                                        cx,
                                                    );
                                                    return;
                                                }

                                                if url.is_empty() {
                                                    window.push_notification(
                                                        "Agent URL is required",
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
                                                    .global::<A2aAgentsModel>()
                                                    .agents()
                                                    .iter()
                                                    .any(|a| a.name == name);
                                                if exists {
                                                    window.push_notification(
                                                        format!("Agent '{}' already exists", name),
                                                        cx,
                                                    );
                                                    return;
                                                }

                                                let api_key = if api_key.is_empty() {
                                                    None
                                                } else {
                                                    Some(api_key)
                                                };

                                                a2a_controller::create_agent(
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

/// Open dialog to edit the API key for an existing agent.
fn show_edit_key_dialog(
    agent_name: String,
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
    let title: SharedString = format!("API Key \u{2014} {}", agent_name).into();

    window.open_dialog(cx, move |dialog, _window, _cx| {
        dialog
            .title(title.clone())
            .overlay(true)
            .keyboard(true)
            .close_button(true)
            .overlay_closable(true)
            .w(px(450.))
            .child(
                div().id("edit-a2a-key-form").child(
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
                                .child(Button::new("cancel-edit-a2a-key").label("Cancel").on_click(
                                    move |_, window, cx| {
                                        window.close_dialog(cx);
                                    },
                                ))
                                .child(
                                    Button::new("save-edit-a2a-key")
                                        .primary()
                                        .label("Save")
                                        .on_click({
                                            let api_key_input = api_key_input.clone();
                                            let agent_name = agent_name.clone();
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

                                                a2a_controller::update_agent_api_key(
                                                    agent_name.clone(),
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

fn local_module_agents_group() -> SettingGroup {
    SettingGroup::new()
        .title("Local Module Agents")
        .description(
            "WASM modules installed in the modules directory that expose agent capabilities. \
             These can be invoked with `/agent <name> <prompt>`. \
             Manage modules in the Modules settings page.",
        )
        .items(vec![SettingItem::render(|_options, _window, cx| {
            let module_agents: Vec<DiscoveredModuleEntry> = cx
                .try_global::<DiscoveredModulesModel>()
                .map(|model| {
                    model
                        .modules
                        .iter()
                        .filter(|m| m.agent && matches!(m.status, ModuleLoadStatus::Loaded))
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();

            if module_agents.is_empty() {
                v_flex()
                    .w_full()
                    .py_6()
                    .items_center()
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("No local module agents found."),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .mt_1()
                            .child(
                                "Install a WASM module with agent capability in the modules \
                                 directory to see it here.",
                            ),
                    )
                    .into_any_element()
            } else {
                v_flex()
                    .w_full()
                    .gap_2()
                    .children(module_agents.iter().map(|module| {
                        let tools_summary = if module.tools.is_empty() {
                            "no tools".to_string()
                        } else {
                            module.tools.join(", ")
                        };
                        let a2a_badge = module.a2a.then(|| {
                            div()
                                .px_1p5()
                                .py_0p5()
                                .rounded_md()
                                .text_xs()
                                .bg(cx.theme().accent)
                                .text_color(cx.theme().accent_foreground)
                                .child("A2A")
                        });

                        h_flex()
                            .w_full()
                            .px_3()
                            .py_2()
                            .rounded_lg()
                            .border_1()
                            .border_color(cx.theme().border)
                            .bg(cx.theme().muted)
                            .gap_3()
                            .child(
                                Icon::new(IconName::Bot)
                                    .size(px(18.))
                                    .text_color(cx.theme().muted_foreground),
                            )
                            .child(
                                v_flex()
                                    .flex_1()
                                    .gap_0p5()
                                    .child(
                                        h_flex()
                                            .gap_2()
                                            .items_center()
                                            .child(
                                                div()
                                                    .text_sm()
                                                    .font_semibold()
                                                    .child(module.name.clone()),
                                            )
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(cx.theme().muted_foreground)
                                                    .child(format!("v{}", module.version)),
                                            )
                                            .when_some(a2a_badge, |this, badge| this.child(badge)),
                                    )
                                    .when(!module.description.is_empty(), |this| {
                                        this.child(
                                            div()
                                                .text_xs()
                                                .text_color(cx.theme().muted_foreground)
                                                .child(module.description.clone()),
                                        )
                                    })
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(format!("Tools: {}", tools_summary)),
                                    ),
                            )
                    }))
                    .into_any_element()
            }
        })])
}
