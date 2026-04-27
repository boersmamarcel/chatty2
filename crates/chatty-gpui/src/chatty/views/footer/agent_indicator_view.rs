use crate::settings::models::extensions_store::{ExtensionKind, ExtensionSource, ExtensionsModel};
use crate::settings::models::{DiscoveredModulesModel, ModuleLoadStatus};
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::popover::Popover;
use gpui_component::{ActiveTheme, Icon, IconName, Sizable, button::*, h_flex};

const AGENT_POPOVER_MIN_WIDTH: f32 = 200.0;
const AGENT_POPOVER_MAX_WIDTH: f32 = 300.0;

/// A single agent entry for display in the footer indicator.
#[derive(Clone)]
struct AgentEntry {
    name: String,
    kind_label: &'static str,
    pricing_model: Option<String>,
    /// `"remote"` / `"remote_only"` → cloud; `"local"` / empty → local
    execution_mode: String,
    enabled: bool,
    /// Extension ID for A2A agents (used for toggle), None for module agents.
    ext_id: Option<String>,
}

#[derive(IntoElement, Default)]
pub struct AgentIndicatorView;

impl AgentIndicatorView {
    pub fn new() -> Self {
        Self
    }
}

impl RenderOnce for AgentIndicatorView {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let mut agents: Vec<AgentEntry> = Vec::new();

        // 1. A2A protocol agents from ExtensionsModel
        let store = cx.global::<ExtensionsModel>();
        for (id, cfg, enabled) in store.all_a2a_agents() {
            let is_external = !cfg.url.contains("localhost") && !cfg.url.contains("127.0.0.1");
            agents.push(AgentEntry {
                name: cfg.name,
                kind_label: "A2A",
                pricing_model: None,
                execution_mode: if is_external {
                    "remote".to_string()
                } else {
                    String::new()
                },
                enabled,
                ext_id: Some(id),
            });
        }

        // 2. WASM module agents from DiscoveredModulesModel
        let discovered_modules = cx.try_global::<DiscoveredModulesModel>();
        let discovered_by_name = discovered_modules
            .as_ref()
            .map(|dm| {
                dm.modules
                    .iter()
                    .map(|module| (module.name.as_str(), module))
                    .collect::<std::collections::HashMap<_, _>>()
            })
            .unwrap_or_default();

        for ext in &store.extensions {
            if !matches!(ext.kind, ExtensionKind::WasmModule) {
                continue;
            }

            let module_name = match &ext.source {
                ExtensionSource::Hive { module_name, .. } => module_name.as_str(),
                ExtensionSource::Custom => ext.id.as_str(),
            };

            if let Some(module) = discovered_by_name.get(module_name)
                && !module.agent
            {
                continue;
            }

            let exec_mode = discovered_by_name
                .get(module_name)
                .map(|m| m.execution_mode.clone())
                .unwrap_or_default();

            agents.push(AgentEntry {
                name: ext.display_name.clone(),
                kind_label: "Agent",
                pricing_model: ext.pricing_model.clone(),
                execution_mode: exec_mode,
                enabled: ext.enabled,
                ext_id: Some(ext.id.clone()),
            });
        }

        /*
         * Keep discovery in the loop only to surface agents that are runtime-loaded
         * but not yet present in the installed extensions store.
         */
        if let Some(dm) = discovered_modules {
            for m in &dm.modules {
                if m.agent
                    && matches!(
                        m.status,
                        ModuleLoadStatus::Loaded | ModuleLoadStatus::Remote
                    )
                {
                    if store.is_installed(&m.name) {
                        continue;
                    }

                    let (name, enabled, pricing_model, ext_id) = store
                        .find(&m.name)
                        .map(|ext| {
                            (
                                ext.display_name.clone(),
                                ext.enabled,
                                ext.pricing_model.clone(),
                                Some(ext.id.clone()),
                            )
                        })
                        .unwrap_or((m.name.clone(), true, None, None));
                    agents.push(AgentEntry {
                        name,
                        kind_label: "Agent",
                        pricing_model,
                        execution_mode: m.execution_mode.clone(),
                        enabled,
                        ext_id,
                    });
                }
            }
        }

        let total_count = agents.len();
        let enabled_count = agents.iter().filter(|a| a.enabled).count();
        let enabled_agents: Vec<_> = agents.iter().filter(|a| a.enabled).collect();
        let summary_label = match enabled_agents.as_slice() {
            [] => "0".to_string(),
            [one] => one.name.clone(),
            [first, ..] => format!("{} +{}", first.name, enabled_count - 1),
        };
        let agent_color = rgb(0x22C55E); // Green-500

        div().when(total_count > 0, |this| {
            let indicator_button = Button::new("agent-indicator")
                .ghost()
                .xsmall()
                .tooltip(format!(
                    "{} agent{} active",
                    enabled_count,
                    if enabled_count == 1 { "" } else { "s" }
                ))
                .child(
                    h_flex()
                        .gap_1()
                        .items_center()
                        .child(
                            Icon::new(IconName::Bot)
                                .size(px(12.0))
                                .text_color(agent_color),
                        )
                        .child(div().text_xs().text_color(agent_color).child(summary_label)),
                );

            this.child(
                Popover::new("agent-list")
                    .trigger(indicator_button)
                    .appearance(false)
                    .content(move |_, _window, cx| {
                        let agents = agents.clone();

                        div()
                            .flex()
                            .flex_col()
                            .bg(cx.theme().background)
                            .border_1()
                            .border_color(cx.theme().border)
                            .rounded_md()
                            .shadow_md()
                            .p_2()
                            .min_w(px(AGENT_POPOVER_MIN_WIDTH))
                            .max_w(px(AGENT_POPOVER_MAX_WIDTH))
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(cx.theme().foreground)
                                    .pb_2()
                                    .child("Agents"),
                            )
                            .child(div().h(px(1.0)).w_full().bg(cx.theme().border).mb_2())
                            .children(
                                agents
                                    .into_iter()
                                    .map(render_agent_item)
                                    .collect::<Vec<_>>(),
                            )
                    }),
            )
        })
    }
}

/// Render a single agent item in the popover.
fn render_agent_item(entry: AgentEntry) -> impl IntoElement {
    let button_id = SharedString::from(format!("toggle-agent-{}", entry.name));
    let ext_id = entry.ext_id.clone();

    div()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .gap_2()
        .px_2()
        .py_1()
        .rounded_md()
        .child(
            div()
                .flex()
                .flex_col()
                .child(div().text_sm().child(entry.name))
                .child(
                    h_flex()
                        .gap_1()
                        .items_center()
                        .child(
                            div()
                                .text_xs()
                                .text_color(gpui::rgb(0x9CA3AF))
                                .child(entry.kind_label),
                        )
                        .when(
                            entry.pricing_model.as_deref() != Some("free")
                                && entry.pricing_model.is_some(),
                            |el| {
                                el.child(
                                    div()
                                        .text_xs()
                                        .px_1()
                                        .rounded_sm()
                                        .bg(gpui::rgb(0xFEF3C7))
                                        .text_color(gpui::rgb(0x92400E))
                                        .child("Paid"),
                                )
                            },
                        )
                        .when(
                            entry.execution_mode == "remote"
                                || entry.execution_mode == "remote_only",
                            |el| {
                                el.child(
                                    div()
                                        .text_xs()
                                        .px_1()
                                        .rounded_sm()
                                        .border_1()
                                        .border_color(gpui::rgb(0x3B82F6))
                                        .text_color(gpui::rgb(0x3B82F6))
                                        .child("☁ Cloud"),
                                )
                            },
                        ),
                ),
        )
        .child(
            Button::new(button_id)
                .xsmall()
                .when(entry.enabled, |btn| btn.primary())
                .when(!entry.enabled, |btn| btn.ghost())
                .child(if entry.enabled { "Active" } else { "Disabled" })
                .when_some(ext_id, |btn, ext_id| {
                    btn.on_click(move |_event, _window, cx| {
                        crate::settings::controllers::extensions_controller::toggle_extension(
                            ext_id.clone(),
                            cx,
                        );
                    })
                }),
        )
}
