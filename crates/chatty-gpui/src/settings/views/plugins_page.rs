//! Settings → Plugins (PL-U5): the WASM plugins in the module directory.
//! A plugin is a set of tools inside an agent — a spec lists it under
//! `[[plugins]]` and grants it capabilities — and never an agent of its
//! own, so each row says which specs use it and what they grant.

use crate::settings::controllers::module_settings_controller;
use crate::settings::models::discovered_modules::DiscoveredModuleEntry;
use crate::settings::models::extensions_store::ExtensionsModel;
use crate::settings::models::{
    AgentSpecsModel, DiscoveredModulesModel, ModuleLoadStatus, ModuleSettingsModel,
};
use crate::settings::views::agent_spec_editor;
use crate::settings::views::extensions_page::trust_badge;
use gpui_component::Sizable;
use gpui_component::button::Button;
use chatty_core::agent_spec::SpecListing;
use chatty_wasm_runtime::SPECLESS_DEFAULTS;
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::setting::{SettingField, SettingGroup, SettingItem, SettingPage};
use gpui_component::switch::Switch;
use gpui_component::{ActiveTheme, h_flex, v_flex};

pub fn plugins_page() -> SettingPage {
    SettingPage::new("Plugins")
        .description(
            "WASM plugins are tools inside an agent: a spec lists a plugin under [[plugins]] \
             and grants it capabilities (Settings → Agents). A plugin is never an agent of its \
             own. Install plugins from the Hive marketplace under Extensions, or copy one into \
             the module directory.",
        )
        .resettable(false)
        .groups(vec![module_runtime_group(), plugins_group()])
}

/// The module runtime switch: what used to require quitting Chatty and
/// hand-editing `module_settings.json`'s `enabled` field, then restarting
/// `toggle_module_runtime` rebuilds the gateway live, so no restart is
/// needed either way. The broker is there whatever it says (AGE-759).
fn module_runtime_group() -> SettingGroup {
    SettingGroup::new().title("Module runtime").items(vec![
        SettingItem::new(
            "Enable module runtime",
            SettingField::switch(
                |cx: &App| cx.global::<ModuleSettingsModel>().enabled,
                |_val: bool, cx: &mut App| {
                    module_settings_controller::toggle_module_runtime(cx);
                },
            )
            .default_value(false),
        )
        .description(
            "Loads WASM modules and serves them on the gateway's HTTP port. Off by default. \
             Delegated agents do not need it: the local agent broker runs either way.",
        ),
    ])
}

fn plugins_group() -> SettingGroup {
    SettingGroup::new()
        .title("Installed plugins")
        .items(vec![SettingItem::render(|_options, _window, cx| {
            let listings = AgentSpecsModel::listings(cx);
            let installed = cx.global::<ExtensionsModel>();
            let status = cx
                .try_global::<DiscoveredModulesModel>()
                .map(|dm| dm.gateway_status.clone())
                .unwrap_or_default();
            let plugins: Vec<_> = cx
                .try_global::<DiscoveredModulesModel>()
                .map(|dm| dm.modules.clone())
                .unwrap_or_default()
                .into_iter()
                .map(|module| {
                    let from_hive = installed.is_installed(&module.name)
                        || installed.is_installed(&module.directory_name);
                    (module, from_hive)
                })
                .collect();

            v_flex()
                .w_full()
                .gap_2()
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(status),
                )
                .when(plugins.is_empty(), |el| {
                    el.child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("No plugins in the module directory."),
                    )
                })
                .children(plugins.into_iter().map(|(module, from_hive)| {
                    let load_error = match &module.status {
                        ModuleLoadStatus::Error(reason) => Some(reason.clone()),
                        _ => None,
                    };
                    let used_by = used_by(&module.name, &listings);
                    let mut facts = vec![format!(
                        "v{} · {}/ · {}",
                        module.version,
                        module.directory_name,
                        if from_hive {
                            "installed from Hive"
                        } else {
                            "copied in by hand"
                        }
                    )];
                    if !module.tools.is_empty() {
                        facts.push(format!("Tools: {}", module.tools.join(", ")));
                    }
                    // What it asks the host for; each spec below grants a
                    // subset (PL-U4).
                    let requested = module.requested.clone().unwrap_or_default();
                    if module.requested.is_some() && requested.is_empty() {
                        facts.push("Requests no capability".to_string());
                    }
                    facts.push(if module.mcp {
                        format!(
                            "Runs: {} · served to MCP clients at /mcp/{}",
                            module.execution_mode, module.name
                        )
                    } else {
                        format!("Runs: {}", module.execution_mode)
                    });
                    facts.push(if used_by.is_empty() {
                        "Used by no agent spec yet: Add to agent… lists it in one.".to_string()
                    } else {
                        format!("Used by: {}", used_by.join("; "))
                    });

                    v_flex()
                        .w_full()
                        .py_1()
                        .child(
                            h_flex()
                                .gap_2()
                                .items_center()
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(cx.theme().foreground)
                                        .child(module.name.clone()),
                                )
                                .when_some(module.trust_level.clone(), |el, trust| {
                                    el.child(trust_badge(&trust, cx))
                                })
                                .child(div().flex_1())
                                .child(add_to_agent_button(&module.name)),
                        )
                        .children(facts.into_iter().map(|fact| {
                            div()
                                .pl_2()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(fact)
                        }))
                        .children(
                            requested
                                .into_iter()
                                .map(|capability| capability_row(&module, &capability, cx)),
                        )
                        .when_some(load_error, |el, reason| {
                            el.child(
                                div()
                                    .pl_2()
                                    .text_xs()
                                    .text_color(cx.theme().danger)
                                    .child(format!("Failed to load: {reason}")),
                            )
                        })
                }))
                .into_any_element()
        })])
}

/// "Add to agent…" for a plugin row (PL-U5b, AGE-718).
pub(crate) fn add_to_agent_button(module: &str) -> Button {
    let name = module.to_string();
    Button::new(SharedString::from(format!("add-to-agent-{module}")))
        .small()
        .outline()
        .label("Add to agent…")
        .on_click(move |_, window, cx| agent_spec_editor::open_add_to_agent(&name, window, cx))
}

/// One requested capability of a module served with no agent spec (SEC-11,
/// AGE-815). `config` and `logging` are on by default; `llm`, `file` and
/// `billing` are off until the user switches them on, which rescans so the
/// gateway links the module against the new set.
fn capability_row(module: &DiscoveredModuleEntry, capability: &str, cx: &App) -> AnyElement {
    let gated =
        !SPECLESS_DEFAULTS.iter().any(|c| c.name() == capability) && capability != "logging";
    let on = !gated || module.granted.iter().any(|g| g == capability);
    let label = format!("Allow {capability}");
    let row = h_flex()
        .pl_2()
        .gap_2()
        .items_center()
        .text_xs()
        .text_color(cx.theme().muted_foreground);
    if !gated {
        return row
            .child(format!("{capability}: on by default"))
            .into_any_element();
    }
    let directory_name = module.directory_name.clone();
    let name = capability.to_string();
    row.child(
        Switch::new(SharedString::from(format!(
            "grant-{}-{capability}",
            module.directory_name
        )))
        .checked(on)
        .label(label)
        .on_click(move |checked: &bool, _window, cx| {
            module_settings_controller::set_module_grant(&directory_name, &name, *checked, cx);
        }),
    )
    .into_any_element()
}

/// The specs that list `module`, each with the capabilities it grants:
/// `auditor (grants llm)`, `auditor (grants nothing)`. Only a
/// name's first definition counts.
fn used_by(module: &str, listings: &[SpecListing]) -> Vec<String> {
    listings
        .iter()
        .filter(|listing| !listing.shadowed)
        .filter_map(|listing| {
            let spec = listing.spec.as_ref().ok()?;
            let plugin = spec.plugins.iter().find(|plugin| plugin.module == module)?;
            Some(format!("{} (grants {})", listing.name, plugin.grant_list()))
        })
        .collect()
}
