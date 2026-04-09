use crate::settings::controllers::extensions_controller;
use crate::settings::models::extensions_store::ExtensionsModel;
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::popover::Popover;
use gpui_component::{ActiveTheme, Icon, IconName, Sizable, button::*, h_flex};

const AGENT_POPOVER_MIN_WIDTH: f32 = 200.0;
const AGENT_POPOVER_MAX_WIDTH: f32 = 300.0;

#[derive(IntoElement, Default)]
pub struct AgentIndicatorView;

impl AgentIndicatorView {
    pub fn new() -> Self {
        Self
    }
}

impl RenderOnce for AgentIndicatorView {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let store = cx.global::<ExtensionsModel>();
        let all_agents = store.all_a2a_agents();
        let total_count = all_agents.len();
        let enabled_count = store.enabled_a2a_count();

        let agent_color = rgb(0x22C55E); // Green-500

        div().when(total_count > 0, |this| {
            let indicator_button = Button::new("agent-indicator")
                .ghost()
                .xsmall()
                .tooltip(format!(
                    "{} agent{} enabled",
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
                        let agents = all_agents.clone();

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
                                    .child("A2A Agents"),
                            )
                            .child(div().h(px(1.0)).w_full().bg(cx.theme().border).mb_2())
                            .children(
                                agents
                                    .into_iter()
                                    .map(|(id, cfg, enabled)| {
                                        render_agent_item(id, cfg.name, enabled)
                                    })
                                    .collect::<Vec<_>>(),
                            )
                    }),
            )
        })
    }
}

/// Render a single agent item in the popover.
fn render_agent_item(ext_id: String, name: String, enabled: bool) -> impl IntoElement {
    let button_id = SharedString::from(format!("toggle-agent-{}", name));

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
                .when(enabled, |btn| btn.primary())
                .when(!enabled, |btn| btn.ghost())
                .child(if enabled { "Enabled" } else { "Disabled" })
                .on_click(move |_event, _window, cx| {
                    extensions_controller::toggle_extension(ext_id.clone(), cx);
                }),
        )
}
