//! The marketplace's Teams tab (MK-T1/T2, AGE-841): search published teams,
//! **Install team** and **Uninstall**. The work is
//! [`chatty_core::team_install`]'s; this moves it off the UI thread and
//! keeps the globals (extensions, the declared roster, the agent listings)
//! and their files in step.

use std::path::PathBuf;

use chatty_core::hive::HiveRegistryClient;
use chatty_core::hive::models::AgentSpecListing;
use chatty_core::team_install::{self, TeamRecord};
use gpui::{App, AsyncApp};
use tracing::{error, info, warn};

use crate::settings::controllers::extensions_controller::hive_session;
use crate::settings::controllers::module_settings_controller;
use crate::settings::models::ModuleSettingsModel;
use crate::settings::models::agent_specs::AgentSpecsModel;
use crate::settings::models::extensions_store::ExtensionsModel;
use crate::settings::models::hive_settings::HiveSettingsModel;
use crate::settings::models::marketplace_state::MarketplaceState;

fn data_dir() -> Option<PathBuf> {
    dirs::data_dir()
}

/// Re-read the installed teams from disk into [`MarketplaceState`].
pub fn refresh_installed_teams(cx: &mut App) {
    let teams = data_dir()
        .map(|dir| team_install::installed_teams(&dir))
        .unwrap_or_default();
    cx.global_mut::<MarketplaceState>().installed_teams = teams;
}

/// Search the published teams; an empty query lists them all.
pub fn search_teams(query: String, cx: &mut App) {
    let registry_url = cx.global::<HiveSettingsModel>().registry_url.clone();
    let client = HiveRegistryClient::new(&registry_url);
    refresh_installed_teams(cx);
    {
        let state = cx.global_mut::<MarketplaceState>();
        state.search_query = query.clone();
        state.set_loading();
    }
    cx.refresh_windows();

    cx.spawn(async move |cx| {
        let result = client.search_agents(&query).await;
        cx.update(|cx| {
            let state = cx.global_mut::<MarketplaceState>();
            match result {
                Ok(list) => {
                    state.loading = false;
                    state.team_results = list.items;
                }
                Err(e) => state.set_error(format!("Team search failed: {e}")),
            }
            cx.refresh_windows();
        })
        .map_err(|e| warn!(error = ?e, "Failed to update UI after team search"))
        .ok();
    })
    .detach();
}

/// Install (or update to) `listing`'s latest version: verify every spec
/// and locked plugin, then write them and add the leader to the roster.
pub fn install_team(listing: AgentSpecListing, cx: &mut App) {
    let Some(data_dir) = data_dir() else {
        cx.global_mut::<MarketplaceState>()
            .set_error("Install failed: no data directory".to_string());
        cx.refresh_windows();
        return;
    };
    let Some(session) = hive_session(cx) else {
        cx.global_mut::<MarketplaceState>().set_error(
            "Sign in to Hive to install a team: its specs are fetched with your account."
                .to_string(),
        );
        cx.refresh_windows();
        return;
    };
    let registry_url = cx.global::<HiveSettingsModel>().registry_url.clone();
    let module_dir = PathBuf::from(&cx.global::<ModuleSettingsModel>().module_dir);
    let client = HiveRegistryClient::new(&registry_url).with_session(session);
    {
        let state = cx.global_mut::<MarketplaceState>();
        state.installing_team = Some(listing.name.clone());
        state.error = None;
        state.team_notice = None;
    }
    cx.refresh_windows();

    cx.spawn(async move |cx: &mut AsyncApp| {
        let fetched =
            team_install::fetch_team(&client, &registry_url, &listing, None, &module_dir).await;
        let applied = cx.update(|cx| {
            cx.global_mut::<MarketplaceState>().installing_team = None;
            let team = fetched?;
            let mut roster = cx.global::<ModuleSettingsModel>().virtual_agents.clone();
            let record = team_install::apply_team(
                team,
                &data_dir,
                &module_dir,
                cx.global_mut::<ExtensionsModel>(),
                &mut roster,
            )?;
            after_change(roster, cx);
            Ok::<TeamRecord, team_install::TeamInstallError>(record)
        });
        let record = match applied {
            Ok(Ok(record)) => record,
            Ok(Err(e)) => {
                error!(error = %e, team = %listing.name, "Team install failed");
                cx.update(|cx| {
                    cx.global_mut::<MarketplaceState>()
                        .set_error(format!("Install failed: {e}"));
                    cx.refresh_windows();
                })
                .ok();
                return;
            }
            Err(e) => {
                warn!(error = ?e, "App gone during a team install");
                return;
            }
        };
        info!(team = %record.leader, version = %record.version, "Installed team");
        if let Err(e) = client
            .record_agent_install(&record.leader, &record.version)
            .await
        {
            warn!(error = %e, "Could not count the team install");
        }
        cx.update(|cx| {
            cx.global_mut::<MarketplaceState>().team_notice = Some(notice(&record));
            cx.refresh_windows();
        })
        .ok();
    })
    .detach();
}

/// Remove the team led by `leader`, and the plugins only it used.
pub fn uninstall_team(leader: String, cx: &mut App) {
    let Some(data_dir) = data_dir() else {
        return;
    };
    let module_dir = PathBuf::from(&cx.global::<ModuleSettingsModel>().module_dir);
    let mut roster = cx.global::<ModuleSettingsModel>().virtual_agents.clone();
    let result = team_install::uninstall_team(
        &leader,
        &data_dir,
        &module_dir,
        cx.global_mut::<ExtensionsModel>(),
        &mut roster,
    );
    match result {
        Ok(record) => {
            info!(team = %leader, "Uninstalled team");
            after_change(roster, cx);
            cx.global_mut::<MarketplaceState>().team_notice = Some(format!(
                "Uninstalled {}: removed {}.",
                record.leader,
                record.specs.join(", ")
            ));
        }
        Err(e) => {
            error!(error = %e, team = %leader, "Team uninstall failed");
            cx.global_mut::<MarketplaceState>()
                .set_error(format!("Uninstall failed: {e}"));
        }
    }
    cx.refresh_windows();
}

/// Persist what an install or uninstall changed and let the roster see it.
fn after_change(roster: Vec<String>, cx: &mut App) {
    let extensions = cx.global::<ExtensionsModel>().clone();
    let roster_changed = cx.global::<ModuleSettingsModel>().virtual_agents != roster;
    cx.global_mut::<ModuleSettingsModel>().virtual_agents = roster;
    let settings = cx.global::<ModuleSettingsModel>().clone();
    cx.spawn(async move |_cx: &mut AsyncApp| {
        if let Err(e) = chatty_core::extensions_repository().save(extensions).await {
            error!(error = ?e, "Failed to save extensions");
        }
        if roster_changed
            && let Err(e) = chatty_core::module_settings_repository()
                .save(settings)
                .await
        {
            error!(error = ?e, "Failed to save module settings");
        }
    })
    .detach();
    refresh_installed_teams(cx);
    AgentSpecsModel::reload(cx);
    module_settings_controller::refresh_runtime(cx);
}

/// What the Teams tab says after an install.
fn notice(record: &TeamRecord) -> String {
    let mut text = format!(
        "Installed {} {} by {} (signature verified): {}. Try `/agent {} …` in a chat.",
        record.leader,
        record.version,
        record.author,
        record.specs.join(", "),
        record.leader
    );
    for module in &record.paid_plugins {
        text += &format!(
            " {module} is paid: its calls need credits and its billing grant in Settings → Plugins."
        );
    }
    text
}
