//! Settings → Plugins (PL-U5): the WASM plugins in the module directory.
//! A plugin is a set of tools inside an agent — a spec lists it under
//! `[[plugins]]` and grants it capabilities — and never an agent of its
//! own, so each row says which specs use it and what they grant.

use crate::settings::models::extensions_store::ExtensionsModel;
use crate::settings::models::{AgentSpecsModel, DiscoveredModulesModel, ModuleLoadStatus};
use crate::settings::views::extensions_page::trust_badge;
use chatty_core::agent_spec::SpecListing;
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::setting::{SettingGroup, SettingItem, SettingPage};
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
        .groups(vec![plugins_group()])
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
                    if let Some(requested) = &module.requested {
                        facts.push(if requested.is_empty() {
                            "Requests no capability".to_string()
                        } else {
                            format!("Requests: {}", requested.join(", "))
                        });
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
                        "Used by no agent spec yet: list it under [[plugins]] in a spec."
                            .to_string()
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
                                }),
                        )
                        .children(facts.into_iter().map(|fact| {
                            div()
                                .pl_2()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(fact)
                        }))
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

/// The specs that list `module`, each with the capabilities it grants:
/// `benford-analyst (grants llm)`, `auditor (grants nothing)`. Only a
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
