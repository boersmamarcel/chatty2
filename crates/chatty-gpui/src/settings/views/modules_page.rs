use crate::settings::controllers::module_settings_controller;
use crate::settings::models::module_settings::{ModuleSettingsModel, default_module_dir};
use crate::settings::models::{DiscoveredModulesModel, ModuleLoadStatus};
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::Button;
use gpui_component::input::{Input, InputState};
use gpui_component::setting::{
    NumberFieldOptions, SettingField, SettingGroup, SettingItem, SettingPage,
};
use gpui_component::{ActiveTheme, Disableable, h_flex, v_flex};

use std::cell::RefCell;
use std::rc::Rc;

pub fn modules_page() -> SettingPage {
    SettingPage::new("Modules")
        .description(
            "WASM modules extend Chatty with custom tools, models, and agents. \
             Enable the module runtime to load modules from a local directory.",
        )
        .resettable(false)
        .groups(vec![
            runtime_group(),
            directory_group(),
            discovered_modules_group(),
        ])
}

fn runtime_group() -> SettingGroup {
    SettingGroup::new()
        .title("Runtime")
        .description("Control the WASM module runtime and protocol gateway.")
        .items(vec![
            SettingItem::new(
                "Enable Module Runtime",
                SettingField::switch(
                    |cx: &App| cx.global::<ModuleSettingsModel>().enabled,
                    |_val: bool, cx: &mut App| {
                        module_settings_controller::toggle_enabled(cx);
                    },
                )
                .default_value(false),
            )
            .description(
                "When enabled, Chatty discovers modules in the configured directory, \
                 registers their tools, models, and agents, and starts the local gateway.",
            ),
            SettingItem::new(
                "Gateway Port",
                SettingField::number_input(
                    NumberFieldOptions {
                        min: 1024.0,
                        max: 65535.0,
                        ..Default::default()
                    },
                    |cx: &App| cx.global::<ModuleSettingsModel>().gateway_port.into(),
                    |val: f64, cx: &mut App| {
                        module_settings_controller::set_gateway_port(val as u16, cx);
                    },
                )
                .default_value(8420.0),
            )
            .description(
                "TCP port for the local protocol gateway (OpenAI, MCP, and A2A endpoints). \
                 Default: 8420.",
            ),
        ])
}

fn directory_group() -> SettingGroup {
    let platform_default = default_module_dir();
    let help_text: SharedString = format!("Platform default: {}", platform_default).into();

    // Persist the InputState across re-renders so typing works.
    let persistent_input: Rc<RefCell<Option<Entity<InputState>>>> =
        Rc::new(RefCell::new(None));

    SettingGroup::new()
        .title("Module Directory")
        .description(
            "The directory Chatty scans for WASM modules on startup. \
             Each sub-directory containing a `module.toml` is loaded as a module.",
        )
        .items(vec![SettingItem::render({
            let persistent_input = persistent_input.clone();
            move |_options, window, cx| {
            let current_dir = cx.global::<ModuleSettingsModel>().module_dir.clone();
            let placeholder: SharedString = default_module_dir().into();

            let input = {
                let existing = persistent_input.borrow().clone();
                existing.unwrap_or_else(|| {
                    let inp = cx.new(|cx| {
                        InputState::new(window, cx)
                            .placeholder(placeholder.clone())
                            .default_value(current_dir)
                    });
                    *persistent_input.borrow_mut() = Some(inp.clone());
                    inp
                })
            };

            let input_clone = input.clone();
            let help = help_text.clone();

            v_flex()
                .w_full()
                .gap_2()
                .child(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .child(Input::new(&input).w_full())
                        .child(
                            gpui_component::button::Button::new("set-module-dir")
                                .label("Apply")
                                .on_click(move |_, _window, cx| {
                                    let dir = input_clone.read(cx).value().trim().to_string();
                                    if !dir.is_empty() {
                                        module_settings_controller::set_module_dir(dir, cx);
                                    }
                                }),
                        )
                        .child(
                            gpui_component::button::Button::new("reset-module-dir")
                                .label("Use Default")
                                .on_click(move |_, _window, cx| {
                                    module_settings_controller::reset_module_dir(cx);
                                }),
                        ),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(help),
                )
                .into_any_element()
        }})])
}

fn discovered_modules_group() -> SettingGroup {
    SettingGroup::new()
        .title("Discovered Modules")
        .description(
            "Modules found in the configured directory. Loaded modules are ready to serve \
             over the local gateway when the runtime is enabled.",
        )
        .items(vec![SettingItem::render(|_options, _window, cx| {
            let state = cx.global::<DiscoveredModulesModel>();
            let modules = state.modules.clone();
            let scan_error = state.scan_error.clone();
            let gateway_status = state.gateway_status.clone();
            let scanned_dir = state.last_scanned_dir.clone();
            let scanning = state.scanning;

            v_flex()
                .w_full()
                .gap_3()
                .child(
                    h_flex()
                        .w_full()
                        .justify_between()
                        .items_center()
                        .child(
                            v_flex()
                                .gap_1()
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(FontWeight::MEDIUM)
                                        .child(format!("Gateway: {gateway_status}")),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(if scanned_dir.is_empty() {
                                            "No module directory scanned yet.".to_string()
                                        } else {
                                            format!("Directory: {scanned_dir}")
                                        }),
                                ),
                        )
                        .child(
                            Button::new("rescan-modules")
                                .label(if scanning { "Scanning…" } else { "Rescan" })
                                .disabled(scanning)
                                .on_click(|_, _, cx| {
                                    module_settings_controller::refresh_runtime(cx);
                                }),
                        ),
                )
                .when_some(scan_error, |this, err| {
                    this.child(
                        div()
                            .rounded_md()
                            .border_1()
                            .border_color(cx.theme().border)
                            .bg(cx.theme().muted)
                            .p_3()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(gpui::red())
                                    .child(err),
                            ),
                    )
                })
                .when(modules.is_empty(), |this| {
                    this.child(
                        v_flex()
                            .w_full()
                            .py_6()
                            .items_center()
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("No modules discovered yet."),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .pt_1()
                                    .child(
                                        "Copy a module folder with module.toml and a .wasm file into the configured directory, then rescan.",
                                    ),
                            ),
                    )
                })
                .when(!modules.is_empty(), |this| {
                    this.child(
                        v_flex()
                            .w_full()
                            .gap_0()
                            .rounded_md()
                            .border_1()
                            .border_color(cx.theme().border)
                            .overflow_hidden()
                            .child(render_modules_header(cx))
                            .children(modules.into_iter().enumerate().map(|(ix, module)| {
                                render_module_row(ix, module, cx).into_any_element()
                            })),
                    )
                })
                .into_any_element()
        })])
}

fn render_modules_header(cx: &App) -> impl IntoElement {
    h_flex()
        .w_full()
        .px_3()
        .py_2()
        .border_b_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().muted)
        .child(
            div()
                .w(px(180.))
                .flex_shrink_0()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(cx.theme().muted_foreground)
                .child("Module"),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(cx.theme().muted_foreground)
                .child("Capabilities"),
        )
        .child(
            div()
                .w(px(180.))
                .flex_shrink_0()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(cx.theme().muted_foreground)
                .child("Status"),
        )
}

fn render_module_row(
    ix: usize,
    module: crate::settings::models::DiscoveredModuleEntry,
    cx: &App,
) -> impl IntoElement {
    let status_text = match &module.status {
        ModuleLoadStatus::Loaded => "Loaded".to_string(),
        ModuleLoadStatus::Error(err) => format!("Error: {err}"),
    };

    let mut capabilities = Vec::new();
    if module.chat {
        capabilities.push("chat");
    }
    if module.agent {
        capabilities.push("agent");
    }
    if !module.tools.is_empty() {
        capabilities.push("tools");
    }

    let mut protocols = Vec::new();
    if module.openai_compat {
        protocols.push("OpenAI");
    }
    if module.mcp {
        protocols.push("MCP");
    }
    if module.a2a {
        protocols.push("A2A");
    }

    let tools_label = if module.tools.is_empty() {
        "No tools".to_string()
    } else {
        format!("Tools: {}", module.tools.join(", "))
    };

    let protocol_label = if protocols.is_empty() {
        "Protocols: none".to_string()
    } else {
        format!("Protocols: {}", protocols.join(", "))
    };

    h_flex()
        .w_full()
        .px_3()
        .py_2()
        .gap_3()
        .items_start()
        .border_b_1()
        .border_color(cx.theme().border)
        .when(ix % 2 == 1, |this| this.bg(cx.theme().muted.opacity(0.35)))
        .child(
            v_flex()
                .w(px(180.))
                .flex_shrink_0()
                .gap_1()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .child(module.name),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!("v{} • {}", module.version, module.directory_name)),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!("WASM: {}", module.wasm_file)),
                ),
        )
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_1()
                .child(div().text_sm().text_color(cx.theme().foreground).child(
                    if module.description.is_empty() {
                        "No description provided.".to_string()
                    } else {
                        module.description
                    },
                ))
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(if capabilities.is_empty() {
                            "Capabilities: none".to_string()
                        } else {
                            format!("Capabilities: {}", capabilities.join(", "))
                        }),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(protocol_label),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(tools_label),
                ),
        )
        .child(
            div()
                .w(px(180.))
                .flex_shrink_0()
                .text_xs()
                .text_color(match module.status {
                    ModuleLoadStatus::Loaded => cx.theme().foreground,
                    ModuleLoadStatus::Error(_) => gpui::red(),
                })
                .child(status_text),
        )
}
