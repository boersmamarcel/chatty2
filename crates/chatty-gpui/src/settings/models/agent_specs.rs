//! The agent specs the desktop can reach, for Settings → Agents, the footer's
//! agent indicator and the start screen (PL-U5).
//!
//! One kind of local agent: a spec, found in the workspace's
//! `.chatty/agents/`, the data directory's `chatty/agents/`, or the presets.
//! Reading them touches the file system, so this caches the listings for the
//! workspace they were read from and re-reads only when the workspace
//! changes or the Agents page asks.

use chatty_core::agent_spec::{SpecListing, inspect_agent_specs, roster_names_of};
use gpui::{App, Global};
use std::path::Path;

use crate::settings::models::{
    DiscoveredModulesModel, ExecutionSettingsModel, ModuleSettingsModel,
};

#[derive(Default)]
pub struct AgentSpecsModel {
    /// The workspace `listings` were read for; `None` before the first read.
    read_for: Option<Option<String>>,
    listings: Vec<SpecListing>,
}

impl Global for AgentSpecsModel {}

fn workspace(cx: &App) -> Option<String> {
    cx.try_global::<ExecutionSettingsModel>()
        .and_then(|settings| settings.workspace_dir.clone())
}

impl AgentSpecsModel {
    /// Every spec file and preset the workspace reaches, re-read if the
    /// workspace changed since the last read.
    pub fn listings(cx: &mut App) -> Vec<SpecListing> {
        let workspace = workspace(cx);
        let model = cx.default_global::<AgentSpecsModel>();
        if model.read_for.as_ref() != Some(&workspace) {
            model.listings = inspect_agent_specs(workspace.as_deref().map(Path::new));
            model.read_for = Some(workspace);
        }
        model.listings.clone()
    }

    /// Re-read the spec directories now (the Agents page's Reload).
    pub fn reload(cx: &mut App) {
        cx.default_global::<AgentSpecsModel>().read_for = None;
        Self::listings(cx);
    }

    /// The last listings read, without touching the file system (for views
    /// that only get `&App`). Empty until something called [`listings`].
    ///
    /// [`listings`]: Self::listings
    pub fn cached(cx: &App) -> Vec<SpecListing> {
        cx.try_global::<AgentSpecsModel>()
            .map(|model| model.listings.clone())
            .unwrap_or_default()
    }
}

/// Whether a broker that serves the roster is there to reach: one is
/// published (lazily, BI-2) or already running, and this platform has the
/// virtual-agent runners it would serve (Unix only). Independent of the
/// module runtime, which gates WASM modules only (AGE-759).
pub fn broker_reachable(cx: &App) -> bool {
    cfg!(unix)
        && cx
            .try_global::<DiscoveredModulesModel>()
            .is_some_and(|d| d.lazy_broker.is_some() || d.gateway.is_some())
}

/// The names the broker serves out of `listings`: what module settings
/// declare, else the default roster: `local-agent`, your own exposed specs,
/// and the presets they delegate to (AGE-760). None when no broker can reach
/// them ([`broker_reachable`]).
pub fn served_names(listings: &[SpecListing], cx: &App) -> Vec<String> {
    if !broker_reachable(cx) {
        return Vec::new();
    }
    let declared = cx
        .try_global::<ModuleSettingsModel>()
        .map(|settings| settings.virtual_agents.clone())
        .unwrap_or_default();
    roster_names_of(&declared, listings)
}
