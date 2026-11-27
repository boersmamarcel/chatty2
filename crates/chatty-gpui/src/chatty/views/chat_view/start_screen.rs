//! Onboarding / "start screen" rendering for `ChatView`.
//!
//! # What lives here
//!
//! - `render_start_screen` — the welcome / capability-summary screen
//!   shown when a conversation has no messages yet.
//! - `render_status_badge` / `summarize_workspace` — helpers used only
//!   by `render_start_screen`.
//! - `render_provider_setup` — the guide shown instead of the capability
//!   summary while no model is set up, so a fresh install says what to do.
//!
//! This is all pure-view code with no behaviour beyond reading global
//! settings. Pulled out because the start screen alone is ~250 lines of
//! nested element builders, which made the main `render` path harder
//! to read.

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::{ActiveTheme, Sizable};
use std::path::{Path, PathBuf};

use crate::settings::models::agent_specs::served_names;
use crate::settings::models::execution_settings::ExecutionSettingsModel;
use crate::settings::models::models_store::ModelsModel;
use crate::settings::models::{
    AgentSpecsModel, DiscoveredModulesModel, ExtensionsModel, ModuleLoadStatus,
    ModuleSettingsModel, SearchSettingsModel,
};

/// Where each provider explains how to get going. Verified 2026-11-27.
const OPENROUTER_KEYS_URL: &str = "https://openrouter.ai/settings/keys";
const OLLAMA_DOWNLOAD_URL: &str = "https://ollama.com/download";
const AZURE_SETUP_URL: &str =
    "https://learn.microsoft.com/en-us/azure/foundry-classic/openai/how-to/create-resource";
const PROVIDERS_GUIDE_URL: &str =
    "https://boersmamarcel.github.io/chatty2/user/providers-and-models.html";

use super::ChatView;

impl ChatView {
    /// Render a desktop-adapted onboarding screen for a new/empty chat.
    pub(super) fn render_start_screen(&self, cx: &Context<Self>) -> impl IntoElement {
        // A fresh install has no model, and nothing else on this screen
        // works without one: say how to get one instead (first-run guide).
        let needs_provider = cx
            .try_global::<ModelsModel>()
            .is_some_and(|models| models.is_loaded() && models.models().is_empty());
        if needs_provider {
            return render_provider_setup(cx).into_any_element();
        }

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
        // A folder that no longer exists is no workspace (AGE-2129): the
        // card showed a deleted /tmp folder as ready.
        let workspace_missing = workspace_dir.as_ref().is_some_and(|dir| !dir.is_dir());
        let workspace_set = workspace_dir.is_some() && !workspace_missing;
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
        let module_runtime_enabled = module_settings.is_some_and(|settings| settings.enabled);
        // The local roster the broker serves, module runtime on or off
        // (PL-U5, AGE-759): specs, never modules. Read from the cache the
        // footer's agent indicator keeps current.
        let local_agent_count = served_names(&AgentSpecsModel::cached(cx), cx).len();

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
                format!("plugins {loaded_module_count}"),
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
                format!("agents {}", enabled_a2a_count + local_agent_count),
                enabled_a2a_count + local_agent_count > 0,
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
                } else if workspace_missing {
                    "workspace missing".to_string()
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
                                    "Ask for a task in plain words. Chatty can read your files, search the web and hand work to other agents.",
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
        .into_any_element()
    }
}

/// The first-run guide: the three providers, what each is for, where to get
/// a key, and where in Settings it goes.
fn render_provider_setup(cx: &App) -> Div {
    let option = |id: &'static str,
                  name: &'static str,
                  blurb: &'static str,
                  hint: Option<&'static str>,
                  link_label: &'static str,
                  url: &'static str| {
        div()
            .w_full()
            .flex()
            .flex_row()
            .items_center()
            .gap_3()
            .py_2()
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(cx.theme().foreground)
                            .child(name),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(blurb),
                    )
                    .when_some(hint, |this, hint| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(hint),
                        )
                    }),
            )
            .child(
                Button::new(id)
                    .label(link_label)
                    .small()
                    .outline()
                    .on_click(move |_, _, cx| cx.open_url(url)),
            )
    };

    div().w_full().flex().justify_center().items_center().child(
        div()
            .w_full()
            .max_w(px(640.))
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
                            .max_w(px(560.))
                            .text_sm()
                            .text_center()
                            .line_height(relative(1.4))
                            .text_color(cx.theme().muted_foreground)
                            .child(
                                "Chatty needs a model to talk to before it can answer. Pick one of these to start:",
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
                    .px_4()
                    .py_2()
                    .flex()
                    .flex_col()
                    .child(option(
                        "setup-openrouter",
                        "OpenRouter",
                        "One key for Claude, GPT, Gemini and hundreds more. You pay per use.",
                        Some("Sign in, add credit, then create a key."),
                        "Get a key ↗",
                        OPENROUTER_KEYS_URL,
                    ))
                    .child(option(
                        "setup-ollama",
                        "Ollama",
                        "Free, and runs on your own computer. No key needed.",
                        Some("Install it, then pull a model in a terminal: ollama pull gemma4"),
                        "Download ↗",
                        OLLAMA_DOWNLOAD_URL,
                    ))
                    .child(option(
                        "setup-azure",
                        "Azure OpenAI",
                        "For an organisation with an Azure subscription.",
                        Some("Create a resource and a deployment, then copy its key and endpoint."),
                        "Setup guide ↗",
                        AZURE_SETUP_URL,
                    )),
            )
            .child(
                div()
                    .max_w(px(560.))
                    .text_sm()
                    .text_center()
                    .line_height(relative(1.4))
                    .text_color(cx.theme().muted_foreground)
                    .child(
                        "Then open Settings → Models & Providers → Manage keys, paste the key (or the Ollama address) and press Save. Its models appear in the picker below.",
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .child(
                        Button::new("setup-open-settings")
                            .label("Open Settings")
                            .small()
                            .primary()
                            .on_click(|_, _, cx| {
                                cx.defer(|cx| {
                                    crate::settings::controllers::SettingsView::open_or_focus_settings_window(cx);
                                });
                            }),
                    )
                    .child(
                        Button::new("setup-read-guide")
                            .label("Step-by-step guide ↗")
                            .small()
                            .ghost()
                            .on_click(|_, _, cx| cx.open_url(PROVIDERS_GUIDE_URL)),
                    ),
            ),
    )
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
