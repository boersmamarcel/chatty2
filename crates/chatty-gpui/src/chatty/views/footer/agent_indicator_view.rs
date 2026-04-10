use crate::settings::models::extensions_store::ExtensionsModel;
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
            agents.push(AgentEntry {
                name: cfg.name,
                kind_label: "A2A",
                enabled,
                ext_id: Some(id),
            });
        }

        // 2. WASM module agents from DiscoveredModulesModel
        if let Some(dm) = cx.try_global::<DiscoveredModulesModel>() {
            for m in &dm.modules {
                if m.agent && matches!(m.status, ModuleLoadStatus::Loaded) {
                    // Look up the extension to get enabled state and ID
                    let (enabled, ext_id) = store
                        .find(&m.name)
                        .map(|ext| (ext.enabled, Some(ext.id.clone())))
                        .unwrap_or((true, None));
                    agents.push(AgentEntry {
                        name: m.name.clone(),
                        kind_label: "Module",
                        enabled,
                        ext_id,
                    });
                }
            }
        }

        let total_count = agents.len();
        let enabled_count = agents.iter().filter(|a| a.enabled).count();
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
                        .child(
                            div()
                                .text_xs()
                                .text_color(agent_color)
                                .child(enabled_count.to_string()),
                        ),
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
                    div()
                        .text_xs()
                        .text_color(gpui::rgb(0x9CA3AF))
                        .child(entry.kind_label),
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
