//! Settings → Agents (PL-U5): every agent spec the workspace reaches, read
//! only. One kind of local agent — a spec, run by the harness — whatever
//! file or preset defined it; the broker serves the roster (AGE-760) at
//! `/a2a/{name}`, and `list_agents`, `invoke_agent` and `/agent` reach them
//! by name. Remote A2A agents are listed below them.

use crate::settings::models::agent_specs::{broker_reachable, served_names};
use crate::settings::models::extensions_store::ExtensionsModel;
use crate::settings::models::{AgentSpecsModel, DiscoveredModulesModel};
use chatty_core::agent_spec::{AgentSpec, SpecListing, SpecSource};
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::*;
use gpui_component::setting::{SettingGroup, SettingItem, SettingPage};
use gpui_component::{ActiveTheme, Sizable, h_flex, v_flex};

pub fn agents_page() -> SettingPage {
    SettingPage::new("Agents")
        .description(
            "Every local agent is a spec: <workspace>/.chatty/agents/<name>.toml, \
             <data dir>/chatty/agents/<name>.toml, or a preset. The broker serves \
             local-agent and your own exposed specs; a preset joins when one of them \
             delegates to it or module settings name it. list_agents, invoke_agent and \
             /agent <name> reach them by name. Edit a spec in its file, then Reload.",
        )
        .resettable(false)
        .groups(vec![specs_group(), remote_agents_group()])
}

fn specs_group() -> SettingGroup {
    SettingGroup::new()
        .title("Agent specs")
        .items(vec![SettingItem::render(|_options, _window, cx| {
            let listings = AgentSpecsModel::listings(cx);
            let served = served_names(&listings, cx);
            // What the conversations' roster actually reaches, whatever the
            // module runtime's switch says (AGE-759).
            let status = if !broker_reachable(cx) {
                "No broker is available here, so none of these are served.".to_string()
            } else if served.is_empty() {
                "The broker serves none of these.".to_string()
            } else {
                format!("Served by the broker: {}", served.join(", "))
            };

            v_flex()
                .w_full()
                .gap_2()
                .child(
                    h_flex()
                        .w_full()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(status),
                        )
                        .child(
                            Button::new("agents-reload")
                                .small()
                                .ghost()
                                .label("Reload")
                                .on_click(|_, _window, cx| AgentSpecsModel::reload(cx)),
                        ),
                )
                .children(
                    served
                        .iter()
                        .filter(|name| !listings.iter().any(|listing| &listing.name == *name))
                        .map(|name| default_worker_row(name, cx)),
                )
                .children(
                    listings
                        .iter()
                        .filter(|listing| !listing.is_team_member_preset())
                        .map(|listing| spec_row(listing, served.contains(&listing.name), cx)),
                )
                .into_any_element()
        })])
}

/// `local-agent` when no file defines it: the bare spec, the leader's
/// tools on the default model.
fn default_worker_row(name: &str, cx: &App) -> AnyElement {
    v_flex()
        .w_full()
        .py_1()
        .child(
            h_flex()
                .gap_2()
                .items_center()
                .child(name_label(name, cx))
                .child(badge("Served", gpui::rgb(0x16A34A).into())),
        )
        .child(detail(
            "The default worker: the default model, the full tool set.",
            cx,
        ))
        .into_any_element()
}

fn spec_row(listing: &SpecListing, served: bool, cx: &App) -> AnyElement {
    // An experimental team's role joins the roster only when asked for
    // (AGE-760): say so, and how.
    let preset_off = !served
        && !listing.shadowed
        && listing.source == SpecSource::Preset
        && listing.spec.as_ref().is_ok_and(|spec| spec.swarm.exposed);
    let (status, color): (&str, Hsla) = match (&listing.spec, listing.shadowed) {
        (_, true) => ("Shadowed", cx.theme().muted_foreground),
        (Err(_), false) => ("Does not load", cx.theme().danger),
        (Ok(_), false) if served => ("Served", gpui::rgb(0x16A34A).into()),
        (Ok(spec), false) if !spec.swarm.exposed => ("Not exposed", cx.theme().muted_foreground),
        (Ok(_), false) if preset_off => ("Preset · not in the roster", cx.theme().muted_foreground),
        (Ok(_), false) => ("Not in the roster", cx.theme().muted_foreground),
    };

    v_flex()
        .w_full()
        .py_1()
        .child(
            h_flex()
                .gap_2()
                .items_center()
                .child(name_label(&listing.name, cx))
                .child(badge(status, color))
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(listing.source.label()),
                ),
        )
        .when(preset_off, |el| {
            el.child(detail(
                "Runs with its team (chatty-tui --team <id>); to add it here, name it in \
                 virtual_agents in module_settings.json.",
                cx,
            ))
        })
        .map(|el| match &listing.spec {
            Ok(spec) => el.children(spec_details(spec, cx)),
            Err(error) => el.child(
                div()
                    .pl_2()
                    .text_xs()
                    .text_color(cx.theme().danger)
                    .child(error.clone()),
            ),
        })
        .into_any_element()
}

/// What a spec declares, one fact per line: description, model and
/// profile, plugins with the capabilities they request and are granted,
/// and who it may call and be called by.
fn spec_details(spec: &AgentSpec, cx: &App) -> Vec<AnyElement> {
    let mut lines = Vec::new();
    if let Some(description) = spec.agent.description.as_deref() {
        lines.push(detail(description, cx));
    }
    let mut run = vec![format!(
        "Model: {}",
        spec.agent.model.as_deref().unwrap_or("the default")
    )];
    run.push(format!(
        "Profile: {}",
        spec.tools.profile.as_deref().unwrap_or("the full tool set")
    ));
    if !spec.tools.disable.is_empty() {
        run.push(format!("Disabled: {}", spec.tools.disable.join(", ")));
    }
    lines.push(detail(&run.join(" · "), cx));
    if !spec.plugins.is_empty() {
        let plugins: Vec<String> = spec
            .plugins
            .iter()
            .map(|plugin| {
                let version = plugin
                    .version
                    .as_deref()
                    .map(|v| format!(" {v}"))
                    .unwrap_or_default();
                // What the loaded plugin requests, against what the spec
                // grants it (PL-U4).
                let requested = cx
                    .try_global::<DiscoveredModulesModel>()
                    .and_then(|dm| dm.modules.iter().find(|m| m.name == plugin.module))
                    .and_then(|module| module.requested.as_deref());
                format!(
                    "{}{version} ({})",
                    plugin.module,
                    plugin.capabilities_summary(requested)
                )
            })
            .collect();
        lines.push(detail(&format!("Plugins: {}", plugins.join("; ")), cx));
    }
    let mut swarm = vec![if spec.swarm.exposed {
        "Exposed".to_string()
    } else {
        "Not exposed".to_string()
    }];
    if !spec.swarm.delegates_to.is_empty() {
        swarm.push(format!(
            "delegates to {}",
            spec.swarm.delegates_to.join(", ")
        ));
    }
    if let Some(callers) = &spec.swarm.callers {
        swarm.push(format!("callers {}", callers.join(", ")));
    }
    lines.push(detail(&swarm.join(" · "), cx));
    lines
}

fn remote_agents_group() -> SettingGroup {
    SettingGroup::new()
        .title("Remote A2A agents")
        .description("Third-party agents, added and toggled under Extensions.")
        .items(vec![SettingItem::render(|_options, _window, cx| {
            let agents = cx
                .try_global::<ExtensionsModel>()
                .map(|model| model.all_a2a_agents())
                .unwrap_or_default();
            v_flex()
                .w_full()
                .gap_1()
                .when(agents.is_empty(), |el| {
                    el.child(detail("No remote agents configured.", cx))
                })
                .children(agents.into_iter().map(|(_, config, enabled)| {
                    h_flex()
                        .gap_2()
                        .items_center()
                        .py_1()
                        .child(name_label(&config.name, cx))
                        .child(badge(
                            if enabled { "Enabled" } else { "Disabled" },
                            cx.theme().muted_foreground,
                        ))
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(config.url.clone()),
                        )
                }))
                .into_any_element()
        })])
}

fn name_label(name: &str, cx: &App) -> Div {
    div()
        .text_sm()
        .font_weight(FontWeight::MEDIUM)
        .text_color(cx.theme().foreground)
        .child(name.to_string())
}

fn detail(text: &str, cx: &App) -> AnyElement {
    div()
        .pl_2()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(text.to_string())
        .into_any_element()
}

fn badge(label: &str, color: Hsla) -> Div {
    div()
        .text_xs()
        .px_1()
        .rounded_sm()
        .border_1()
        .border_color(color)
        .text_color(color)
        .child(label.to_string())
}
