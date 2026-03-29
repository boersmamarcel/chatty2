use crate::settings::controllers::module_settings_controller;
use crate::settings::models::module_settings::{ModuleSettingsModel, default_module_dir};
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::input::{Input, InputState};
use gpui_component::setting::{
    NumberFieldOptions, SettingField, SettingGroup, SettingItem, SettingPage,
};
use gpui_component::{ActiveTheme, h_flex, v_flex};

pub fn modules_page() -> SettingPage {
    SettingPage::new("Modules")
        .description(
            "WASM modules extend Chatty with custom tools, models, and agents. \
             Enable the module runtime to load modules from a local directory.",
        )
        .resettable(false)
        .groups(vec![runtime_group(), directory_group()])
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

    SettingGroup::new()
        .title("Module Directory")
        .description(
            "The directory Chatty scans for WASM modules on startup. \
             Each sub-directory containing a `module.toml` is loaded as a module.",
        )
        .items(vec![SettingItem::render(move |_options, window, cx| {
            let current_dir = cx.global::<ModuleSettingsModel>().module_dir.clone();
            let placeholder: SharedString = default_module_dir().into();

            let input = cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(placeholder)
                    .default_value(current_dir)
            });

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
                        ),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(help),
                )
                .into_any_element()
        })])
}
