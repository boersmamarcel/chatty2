use crate::assets::CustomIcon;
use crate::settings::controllers::execution_settings_controller;
use crate::settings::models::execution_settings::ExecutionSettingsModel;
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::popover::Popover;
use gpui_component::{ActiveTheme, Disableable, Icon, Sizable, button::*, h_flex};

// Popover dimensions (same as MCP indicator)
const TOOLS_POPOVER_MIN_WIDTH: f32 = 200.0;
const TOOLS_POPOVER_MAX_WIDTH: f32 = 300.0;

#[derive(IntoElement, Default)]
pub struct ToolsIndicatorView;

impl ToolsIndicatorView {
    pub fn new() -> Self {
        Self
    }
}

impl RenderOnce for ToolsIndicatorView {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let settings = cx.global::<ExecutionSettingsModel>();
        let enabled_count = count_enabled_categories(settings);

        // Amber color for tools/construction theme (distinct from MCP blue)
        let tools_color = rgb(0xF59E0B); // Amber-500

        div().child({
            // Main indicator button
            let indicator_button = Button::new("tools-indicator")
                .ghost()
                .xsmall()
                .tooltip(format!(
                    "{} tool categor{} enabled",
                    enabled_count,
                    if enabled_count == 1 { "y" } else { "ies" }
                ))
                .child(
                    h_flex()
                        .gap_1()
                        .items_center()
                        .child(
                            Icon::new(CustomIcon::Wrench)
                                .size(px(12.0))
                                .text_color(tools_color),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(tools_color)
                                .child(enabled_count.to_string()),
                        ),
                );

            // Popover with tool categories
            Popover::new("tools-list")
                .trigger(indicator_button)
                .appearance(false)
                .content(move |_, _window, cx| {
                    let settings = cx.global::<ExecutionSettingsModel>();
                    let bash_enabled = settings.enabled;
                    let workspace_set = settings.workspace_dir.is_some();
                    let fs_read_enabled = settings.filesystem_read_enabled;
                    let fs_write_enabled = settings.filesystem_write_enabled;

                    div()
                        .flex()
                        .flex_col()
                        .bg(cx.theme().background)
                        .border_1()
                        .border_color(cx.theme().border)
                        .rounded_md()
                        .shadow_md()
                        .p_2()
                        .min_w(px(TOOLS_POPOVER_MIN_WIDTH))
                        .max_w(px(TOOLS_POPOVER_MAX_WIDTH))
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::BOLD)
                                .text_color(cx.theme().foreground)
                                .pb_2()
                                .child("Filesystem Tools"),
                        )
                        .child(div().h(px(1.0)).w_full().bg(cx.theme().border).mb_2())
                        // Bash Execution - always toggleable
                        .child(render_bash_item(bash_enabled, cx))
                        // Filesystem Read - toggleable only if workspace_dir is set
                        .child(render_filesystem_toggle_item(
                            "Filesystem Read",
                            fs_read_enabled,
                            workspace_set,
                            "toggle-fs-read",
                            execution_settings_controller::toggle_filesystem_read,
                        ))
                        // Filesystem Write - toggleable only if workspace_dir is set
                        .child(render_filesystem_toggle_item(
                            "Filesystem Write",
                            fs_write_enabled,
                            workspace_set,
                            "toggle-fs-write",
                            execution_settings_controller::toggle_filesystem_write,
                        ))
                        // Hint when workspace is not configured
                        .when(!workspace_set, |this| {
                            this.child(
                                div()
                                    .h(px(1.0))
                                    .w_full()
                                    .bg(cx.theme().border)
                                    .mt_2()
                                    .mb_2(),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .px_2()
                                    .child("ℹ Configure workspace in Settings to enable filesystem tools"),
                            )
                        })
                        // Always show hint about new conversation requirement
                        .child(
                            div()
                                .h(px(1.0))
                                .w_full()
                                .bg(cx.theme().border)
                                .mt_2()
                                .mb_2(),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .px_2()
                                .child("ℹ Start a new conversation after changing tool settings"),
                        )
                })
        })
    }
}

/// Count enabled tool categories (0-3)
fn count_enabled_categories(settings: &ExecutionSettingsModel) -> usize {
    let mut count = 0;
    if settings.enabled {
        count += 1; // Bash execution
    }
    if settings.workspace_dir.is_some() && settings.filesystem_read_enabled {
        count += 1; // Filesystem Read
    }
    if settings.workspace_dir.is_some() && settings.filesystem_write_enabled {
        count += 1; // Filesystem Write
    }
    count
}

/// Render the bash execution toggle button
fn render_bash_item(enabled: bool, _cx: &App) -> impl IntoElement {
    let button_id = SharedString::from("toggle-bash");

    div()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .gap_2()
        .px_2()
        .py_1()
        .rounded_md()
        .child(div().text_sm().child("Bash Execution"))
        .child(
            Button::new(button_id)
                .xsmall()
                .when(enabled, |btn| btn.primary())
                .when(!enabled, |btn| btn.ghost())
                .child(if enabled { "Enabled" } else { "Disabled" })
                .on_click(move |_event, _window, cx| {
                    execution_settings_controller::toggle_execution(cx);
                }),
        )
}

/// Render a toggleable filesystem tool item (disabled if workspace not set)
fn render_filesystem_toggle_item(
    name: &str,
    enabled: bool,
    workspace_set: bool,
    button_id: &str,
    toggle_fn: fn(&mut App),
) -> impl IntoElement {
    let name = name.to_string();
    let button_id = SharedString::from(button_id.to_string());

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
                .when(workspace_set && enabled, |btn| btn.primary())
                .when(!workspace_set || !enabled, |btn| btn.ghost())
                .when(!workspace_set, |btn| btn.disabled(true))
                .child(if !workspace_set {
                    "Not Configured"
                } else if enabled {
                    "Enabled"
                } else {
                    "Disabled"
                })
                .on_click(move |_event, _window, cx| {
                    if workspace_set {
                        toggle_fn(cx);
                    }
                }),
        )
}
