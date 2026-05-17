//! Onboarding / "start screen" rendering for `ChatView`.
//!
//! # What lives here
//!
//! - `render_loading_skeleton` — placeholder shown while messages load.
//! - `render_start_screen` — the welcome / capability-summary screen
//!   shown when a conversation has no messages yet.
//! - `render_status_badge` / `summarize_workspace` — helpers used only
//!   by `render_start_screen`.
//!
//! This is all pure-view code with no behaviour beyond reading global
//! settings. Pulled out because the start screen alone is ~250 lines of
//! nested element builders, which made the main `render` path harder
//! to read.

use gpui::*;
use gpui_component::ActiveTheme;
use gpui_component::skeleton::Skeleton;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::settings::models::execution_settings::ExecutionSettingsModel;
use crate::settings::models::{
    DiscoveredModulesModel, ExtensionsModel, ModuleLoadStatus, ModuleSettingsModel,
    SearchSettingsModel,
};

use super::ChatView;

impl ChatView {
    /// Render loading skeleton indicator
    pub(super) fn render_loading_skeleton(&self) -> impl IntoElement {
        div()
            .p_4()
            .flex()
            .flex_col()
            .gap_2()
            .child(Skeleton::new().w(px(280.)).h(px(16.)).rounded(px(4.)))
            .child(Skeleton::new().w(px(220.)).h(px(16.)).rounded(px(4.)))
            .child(Skeleton::new().w(px(180.)).h(px(16.)).rounded(px(4.)))
    }

    /// Render a desktop-adapted onboarding screen for a new/empty chat.
    pub(super) fn render_start_screen(&self, cx: &Context<Self>) -> impl IntoElement {
        let (workspace_override, skill_count) = {
            let input = self.chat_input_state.read(cx);
            (input.working_dir().cloned(), input.available_skills().len())
        };

        let execution_settings = cx.try_global::<ExecutionSettingsModel>();
        let search_settings = cx.try_global::<SearchSettingsModel>();
        let extensions_model = cx.try_global::<ExtensionsModel>();
        let module_settings = cx.try_global::<ModuleSettingsModel>();
        let discovered_modules = cx.try_global::<DiscoveredModulesModel>();

        let workspace_dir = workspace_override.or_else(|| {
            execution_settings
                .and_then(|settings| settings.workspace_dir.clone().map(PathBuf::from))
        });
        let workspace_set = workspace_dir.is_some();
        let fs_read_enabled = execution_settings
            .is_some_and(|settings| workspace_set && settings.filesystem_read_enabled);
        let fs_write_enabled = execution_settings
            .is_some_and(|settings| workspace_set && settings.filesystem_write_enabled);
        let fetch_enabled = execution_settings.is_some_and(|settings| settings.fetch_enabled);
        let memory_enabled = execution_settings.is_some_and(|settings| settings.memory_enabled)
            && cx
                .try_global::<crate::chatty::services::MemoryService>()
                .is_some();
        let semantic_memory_enabled = execution_settings
            .is_some_and(|settings| settings.embedding_enabled)
            && cx
                .try_global::<chatty_core::services::EmbeddingService>()
                .is_some();

        let search_enabled = search_settings.is_some_and(|settings| settings.enabled);
        let browser_use_enabled = search_settings.is_some_and(|settings| {
            settings.browser_use_enabled
                && settings
                    .browser_use_api_key
                    .as_ref()
                    .is_some_and(|key| !key.trim().is_empty())
        });
        let daytona_enabled = search_settings.is_some_and(|settings| {
            settings.daytona_enabled
                && settings
                    .daytona_api_key
                    .as_ref()
                    .is_some_and(|key| !key.trim().is_empty())
        });

        let enabled_mcp_count = extensions_model
            .map(|model| model.enabled_mcp_count())
            .unwrap_or(0);
        let enabled_a2a_count = extensions_model
            .map(|model| {
                model
                    .all_a2a_agents()
                    .into_iter()
                    .filter(|(_, _, enabled)| *enabled)
                    .count()
            })
            .unwrap_or(0);
        let enabled_module_ids: HashSet<String> = extensions_model
            .map(|model| {
                model
                    .wasm_module_ids()
                    .into_iter()
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let loaded_module_count = discovered_modules
            .map(|model| {
                model
                    .modules
                    .iter()
                    .filter(|module| {
                        matches!(
                            module.status,
                            ModuleLoadStatus::Loaded | ModuleLoadStatus::Remote
                        )
                    })
                    .count()
            })
            .unwrap_or(0);
        let enabled_module_agent_count = discovered_modules
            .map(|model| {
                model
                    .modules
                    .iter()
                    .filter(|module| {
                        module.agent
                            && matches!(
                                module.status,
                                ModuleLoadStatus::Loaded | ModuleLoadStatus::Remote
                            )
                            && enabled_module_ids.contains(module.name.as_str())
                    })
                    .count()
            })
            .unwrap_or(0);
        let module_runtime_enabled = module_settings.is_some_and(|settings| settings.enabled);

        let summary_badges = vec![
            render_status_badge(
                if skill_count == 1 {
                    "1 skill".to_string()
                } else {
                    format!("{skill_count} skills")
                },
                skill_count > 0,
                rgb(0x22C55E),
                cx,
            )
            .into_any_element(),
            render_status_badge(
                format!("modules {loaded_module_count}"),
                module_runtime_enabled && loaded_module_count > 0,
                rgb(0xA855F7),
                cx,
            )
            .into_any_element(),
            render_status_badge(
                format!("MCP {enabled_mcp_count}"),
                enabled_mcp_count > 0,
                rgb(0x3B82F6),
                cx,
            )
            .into_any_element(),
            render_status_badge(
                format!("agents {}", enabled_a2a_count + enabled_module_agent_count),
                enabled_a2a_count + enabled_module_agent_count > 0,
                rgb(0x14B8A6),
                cx,
            )
            .into_any_element(),
        ];

        let capability_badges = vec![
            render_status_badge(
                "files",
                fs_read_enabled || fs_write_enabled,
                rgb(0x3B82F6),
                cx,
            )
            .into_any_element(),
            render_status_badge(
                "web",
                fetch_enabled || search_enabled || browser_use_enabled || daytona_enabled,
                rgb(0x2563EB),
                cx,
            )
            .into_any_element(),
            render_status_badge(
                "memory",
                memory_enabled || semantic_memory_enabled,
                rgb(0x10B981),
                cx,
            )
            .into_any_element(),
            render_status_badge(
                if workspace_set {
                    format!(
                        "workspace {}",
                        workspace_dir
                            .as_ref()
                            .map(|path| summarize_workspace(path))
                            .unwrap_or_else(|| "ready".to_string())
                    )
                } else {
                    "workspace needed".to_string()
                },
                workspace_set,
                rgb(0xF59E0B),
                cx,
            )
            .into_any_element(),
        ];

        let ideas = if workspace_set {
            "Ask for a task, attach files with @, or lean on skills, MCP, modules, and web-enabled tools."
        } else {
            "Ask for a task, and add a workspace when you want project-aware file tools and local capabilities."
        };

        div().w_full().flex().justify_center().items_center().child(
            div()
                .w_full()
                .max_w(px(760.))
                .px_4()
                .py_6()
                .flex()
                .flex_col()
                .items_center()
                .gap_4()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .text_xl()
                                .font_weight(FontWeight::BOLD)
                                .text_color(cx.theme().foreground)
                                .child("Welcome to Chatty"),
                        )
                        .child(
                            div()
                                .max_w(px(620.))
                                .text_sm()
                                .text_center()
                                .line_height(relative(1.4))
                                .text_color(cx.theme().muted_foreground)
                                .child(
                                    "A desktop AI workspace with live skills, tools, modules, MCP servers, agents, and web-connected workflows.",
                                ),
                        ),
                )
                .child(
                    div()
                        .w_full()
                        .rounded_lg()
                        .border_1()
                        .border_color(cx.theme().border)
                        .bg(cx.theme().secondary)
                        .p_4()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(
                            div()
                                .w_full()
                                .flex()
                                .flex_row()
                                .flex_wrap()
                                .justify_center()
                                .gap_2()
                                .children(summary_badges),
                        )
                        .child(
                            div()
                                .w_full()
                                .flex()
                                .flex_row()
                                .flex_wrap()
                                .justify_center()
                                .gap_2()
                                .children(capability_badges),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_center()
                                .line_height(relative(1.4))
                                .text_color(cx.theme().muted_foreground)
                                .child(ideas.to_string()),
                        ),
                ),
        )
    }
}

fn render_status_badge(
    label: impl Into<String>,
    enabled: bool,
    accent: impl Into<Hsla>,
    cx: &App,
) -> Div {
    let accent = accent.into();
    let background = if enabled {
        accent.opacity(0.14)
    } else {
        cx.theme().background
    };
    let border = if enabled {
        accent.opacity(0.35)
    } else {
        cx.theme().border
    };
    let foreground = if enabled {
        accent
    } else {
        cx.theme().muted_foreground
    };

    div()
        .px_2()
        .py_1()
        .rounded_full()
        .border_1()
        .border_color(border)
        .bg(background)
        .text_xs()
        .text_color(foreground)
        .child(label.into())
}

fn summarize_workspace(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map(|name| name.to_string())
        .unwrap_or_else(|| path.to_string_lossy().to_string())
}
