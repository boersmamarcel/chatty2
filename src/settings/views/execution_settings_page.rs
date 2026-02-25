use crate::settings::controllers::execution_settings_controller;
use crate::settings::models::execution_settings::{ApprovalMode, ExecutionSettingsModel};
use gpui::{App, IntoElement, ParentElement, SharedString, Styled, div};
use gpui_component::{
    ActiveTheme,
    button::Button,
    menu::{DropdownMenu, PopupMenuItem},
    setting::{NumberFieldOptions, SettingField, SettingGroup, SettingItem, SettingPage},
};

pub fn execution_settings_page() -> SettingPage {
    SettingPage::new("Code Execution")
        .description("Configure code execution and filesystem access")
        .resettable(false)
        .groups(vec![
            SettingGroup::new()
                .title("Security Settings")
                .description(
                    "⚠️ Enabling code execution allows the AI to run shell commands. \
                     Commands will require approval based on your approval mode setting.",
                )
                .items(vec![
                    SettingItem::new(
                        "Enable Code Execution",
                        SettingField::switch(
                            |cx: &App| cx.global::<ExecutionSettingsModel>().enabled,
                            |_val: bool, cx: &mut App| {
                                execution_settings_controller::toggle_execution(cx);
                            },
                        )
                        .default_value(false),
                    )
                    .description("Master toggle for bash shell command execution"),
                    SettingItem::new(
                        "Allow LLM to Manage MCP Servers",
                        SettingField::switch(
                            |cx: &App| cx.global::<ExecutionSettingsModel>().mcp_service_tool_enabled,
                            |_val: bool, cx: &mut App| {
                                execution_settings_controller::toggle_mcp_service_tool(cx);
                            },
                        )
                        .default_value(false),
                    )
                    .description(
                        "When enabled, the AI can add, edit, and delete MCP servers via the \
                         add_mcp_service, edit_mcp_service, and delete_mcp_service tools. \
                         Disable to prevent the AI from modifying MCP server configurations.",
                    ),
                    SettingItem::new(
                        "Enable Web Fetch",
                        SettingField::switch(
                            |cx: &App| cx.global::<ExecutionSettingsModel>().fetch_enabled,
                            |_val: bool, cx: &mut App| {
                                execution_settings_controller::toggle_fetch(cx);
                            },
                        )
                        .default_value(true),
                    )
                    .description(
                        "Built-in read-only web fetch tool. Allows the AI to retrieve web pages \
                         and API responses without requiring an MCP fetch server.",
                    ),
                    SettingItem::new(
                        "Enable Git Integration",
                        SettingField::switch(
                            |cx: &App| cx.global::<ExecutionSettingsModel>().git_enabled,
                            |_val: bool, cx: &mut App| {
                                execution_settings_controller::toggle_git(cx);
                            },
                        )
                        .default_value(false),
                    )
                    .description(
                        "Git tools for repository operations (status, diff, log, branch, commit). \
                         Requires a workspace directory that is a git repository. Write operations \
                         (commit, branch) require user confirmation.",
                    ),
                    SettingItem::new(
                        "Approval Mode",
                        SettingField::render(|_options, _window, cx| {
                            let current_mode = cx.global::<ExecutionSettingsModel>().approval_mode.clone();
                            let current_label = match current_mode {
                                ApprovalMode::AlwaysAsk => "Always Ask (Safest)",
                                ApprovalMode::AutoApproveSandboxed => "Auto-approve Sandboxed",
                                ApprovalMode::AutoApproveAll => "Auto-approve All (Dangerous)",
                            };

                            Button::new("approval-mode-dropdown")
                                .label(current_label)
                                .dropdown_caret(true)
                                .outline()
                                .w_full()
                                .dropdown_menu_with_anchor(
                                    gpui::Corner::BottomLeft,
                                    move |menu, _, _| {
                                        menu.item(
                                            PopupMenuItem::new("Always Ask (Safest)")
                                                .checked(matches!(
                                                    current_mode,
                                                    ApprovalMode::AlwaysAsk
                                                ))
                                                .on_click(|_, _, cx| {
                                                    execution_settings_controller::set_approval_mode(
                                                        ApprovalMode::AlwaysAsk,
                                                        cx,
                                                    );
                                                }),
                                        )
                                        .item(
                                            PopupMenuItem::new("Auto-approve Sandboxed")
                                                .checked(matches!(
                                                    current_mode,
                                                    ApprovalMode::AutoApproveSandboxed
                                                ))
                                                .on_click(|_, _, cx| {
                                                    execution_settings_controller::set_approval_mode(
                                                        ApprovalMode::AutoApproveSandboxed,
                                                        cx,
                                                    );
                                                }),
                                        )
                                        .item(
                                            PopupMenuItem::new("Auto-approve All (Dangerous)")
                                                .checked(matches!(
                                                    current_mode,
                                                    ApprovalMode::AutoApproveAll
                                                ))
                                                .on_click(|_, _, cx| {
                                                    execution_settings_controller::set_approval_mode(
                                                        ApprovalMode::AutoApproveAll,
                                                        cx,
                                                    );
                                                }),
                                        )
                                    },
                                )
                                .into_any_element()
                        }),
                    )
                    .description(
                        "How to handle command execution requests: \
                         Always Ask requires confirmation for every command. \
                         Auto-approve Sandboxed automatically allows safe commands. \
                         Auto-approve All runs all commands without asking (use with caution).",
                    ),
                ]),
            SettingGroup::new()
                .title("Filesystem Access")
                .description("Configure workspace directory for file read/write operations")
                .items(vec![
                    SettingItem::new(
                        "Workspace Directory",
                        SettingField::input(
                            |cx: &App| {
                                cx.global::<ExecutionSettingsModel>()
                                    .workspace_dir
                                    .clone()
                                    .unwrap_or_default()
                                    .into()
                            },
                            |val: SharedString, cx: &mut App| {
                                let workspace_dir = if val.is_empty() {
                                    None
                                } else {
                                    Some(val.to_string())
                                };
                                execution_settings_controller::set_workspace_dir(workspace_dir, cx);
                            },
                        ),
                    )
                    .description("Optional directory path for file operations. Leave empty to disable filesystem tools."),
                ]),
            SettingGroup::new()
                .title("Execution Limits")
                .description("Resource limits for code execution")
                .items(vec![
                    SettingItem::new(
                        "Max Agent Turns",
                        SettingField::number_input(
                            NumberFieldOptions {
                                min: 1.0,
                                max: 100.0,
                                ..Default::default()
                            },
                            |cx: &App| {
                                cx.global::<ExecutionSettingsModel>().max_agent_turns as f64
                            },
                            |val: f64, cx: &mut App| {
                                execution_settings_controller::set_max_agent_turns(
                                    val as u32, cx,
                                );
                            },
                        )
                        .default_value(10.0),
                    )
                    .description(
                        "Maximum number of tool-call rounds the agent can perform per response. \
                         Higher values allow the agent to complete more complex multi-step tasks.",
                    ),
                    SettingItem::new(
                        "Timeout",
                        SettingField::render(|_options, _window, cx| {
                            let timeout = cx.global::<ExecutionSettingsModel>().timeout_seconds;
                            div()
                                .child(format!("{} seconds", timeout))
                                .text_color(cx.theme().muted_foreground)
                                .into_any_element()
                        }),
                    )
                    .description("Maximum execution time for commands"),
                    SettingItem::new(
                        "Max Output",
                        SettingField::render(|_options, _window, cx| {
                            let max_bytes = cx.global::<ExecutionSettingsModel>().max_output_bytes;
                            let kb = max_bytes / 1024;
                            div()
                                .child(format!("{} KB", kb))
                                .text_color(cx.theme().muted_foreground)
                                .into_any_element()
                        }),
                    )
                    .description("Maximum output size to prevent memory exhaustion"),
                    SettingItem::new(
                        "Network Isolation",
                        SettingField::render(|_options, _window, cx| {
                            let enabled = cx.global::<ExecutionSettingsModel>().network_isolation;
                            div()
                                .child(if enabled { "Enabled" } else { "Disabled" })
                                .text_color(cx.theme().muted_foreground)
                                .into_any_element()
                        }),
                    )
                    .description("Enable network isolation in sandbox (when available)"),
                ]),
        ])
}
