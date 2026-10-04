//! Installing an agent team from the Hive marketplace (MK-T1, AGE-841).
//!
//! A team is a published spec (its leader) and the specs it `delegates_to`
//! that the same publisher published (its members, as the registry lists
//! them). Installing it:
//!
//! 1. fetches the leader's spec and each member's;
//! 2. verifies every spec's signature chain against the trusted registry
//!    roots (SEC-3) and that each names the version asked for;
//! 3. downloads each locked plugin at its exact version, through the
//!    signature-checked plugin path ([`install::download_wasm_module`]), and
//!    refuses bytes whose SHA-256 is not the one the signed lockfile pins;
//! 4. writes the specs to the global agents folder
//!    (`<data_dir>/chatty/agents/`), so the leader joins the default roster
//!    (AGE-760) — and, when module settings declare a roster, adds the
//!    leader and the names it delegates to there;
//! 5. leaves a [`TeamRecord`] in `<data_dir>/chatty/installed-teams/`, which
//!    is what [`uninstall_team`] removes by.
//!
//! [`fetch_team`] does the network part and writes nothing; [`apply_team`]
//! writes. Installing again updates the team to the new version. Paid teams
//! (ADR-0024) are refused until per-run fees exist.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use hive_client::HiveRegistryClient;
use hive_client::models::{AgentSpecListing, DownloadResult};
use hive_client::verify::{LockedPlugin, verify_spec_any};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::agent_spec::{AgentSpec, load_agent_spec_from};
use crate::install::{self, InstallError};
use crate::settings::models::extensions_store::{ExtensionSource, ExtensionsModel};
use chatty_module_registry::InstallRecord;

/// Where team records live, under `<data_dir>/chatty/`.
pub const TEAMS_RECORD_DIR: &str = "installed-teams";

#[derive(Debug, Error)]
pub enum TeamInstallError {
    #[error("no published team named '{0}' in the registry")]
    NotFound(String),
    #[error(
        "'{name}' is a paid team ({pricing}). Paid teams are not available yet; \
         only free teams can be installed for now"
    )]
    PaidTeam { name: String, pricing: String },
    #[error("'{name}@{version}' did not verify: {reason}")]
    Unverified {
        name: String,
        version: String,
        reason: String,
    },
    #[error("'{name}@{version}' is not a valid agent spec: {reason}")]
    BadSpec {
        name: String,
        version: String,
        reason: String,
    },
    #[error(
        "plugin '{module}@{version}' does not match the team's lockfile: \
         its SHA-256 is {got}, the signed lock pins {expected}"
    )]
    LockMismatch {
        module: String,
        version: String,
        expected: String,
        got: String,
    },
    #[error(
        "an agent spec named '{name}' already exists at {path}{owner}; remove or rename it first"
    )]
    NameTaken {
        name: String,
        path: String,
        owner: String,
    },
    #[error("no installed team named '{0}'")]
    NotInstalled(String),
    #[error("{0}")]
    Install(#[from] InstallError),
    #[error("registry: {0}")]
    Client(#[from] hive_client::ClientError),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// One spec of a fetched team, verified.
#[derive(Clone, Debug)]
pub struct FetchedSpec {
    pub name: String,
    pub version: String,
    pub spec: AgentSpec,
    pub lockfile: Vec<LockedPlugin>,
}

/// A locked plugin of a fetched team: downloaded and checked against the
/// lock, or already installed with the locked bytes.
#[derive(Debug)]
pub struct FetchedPlugin {
    pub locked: LockedPlugin,
    pub pricing_model: String,
    /// `None` when the module directory already holds exactly these bytes.
    pub download: Option<DownloadResult>,
}

/// Everything [`apply_team`] writes, verified and downloaded.
#[derive(Debug)]
pub struct FetchedTeam {
    pub leader: String,
    pub version: String,
    pub author: String,
    pub registry_url: String,
    /// The leader first, then its members.
    pub specs: Vec<FetchedSpec>,
    pub plugins: Vec<FetchedPlugin>,
}

impl FetchedTeam {
    /// The team's paid plugins: each needs credits and its billing grant
    /// (AGE-815) before a call goes through.
    pub fn paid_plugins(&self) -> Vec<String> {
        self.plugins
            .iter()
            .filter(|p| p.pricing_model != "free")
            .map(|p| p.locked.module.clone())
            .collect()
    }
}

/// An installed team, as `<data_dir>/chatty/installed-teams/<leader>.json`
/// remembers it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TeamRecord {
    pub leader: String,
    pub version: String,
    pub author: String,
    pub registry_url: String,
    /// The spec files written, leader first.
    pub specs: Vec<String>,
    /// Every locked plugin, as `module@version`.
    pub plugins: Vec<String>,
    /// The plugins a team installed (this one, or another it shares them
    /// with), not ones the user had: what uninstalling may remove once no
    /// other team uses them.
    pub installed_plugins: Vec<String>,
    /// The names this team added to a declared roster.
    #[serde(default)]
    pub roster_added: Vec<String>,
    /// Paid plugins, which need the billing grant.
    #[serde(default)]
    pub paid_plugins: Vec<String>,
}

impl TeamRecord {
    fn uses_plugin(&self, module: &str) -> bool {
        self.plugins
            .iter()
            .any(|p| p.split_once('@').map_or(p.as_str(), |(m, _)| m) == module)
    }
}

/// Parse `name[@version]`.
pub fn parse_team_ref(text: &str) -> (String, Option<String>) {
    match text.split_once('@') {
        Some((name, version)) => (name.trim().to_string(), Some(version.trim().to_string())),
        None => (text.trim().to_string(), None),
    }
}

/// The marketplace listing of `name`: the one exact match of a search.
pub async fn find_listing(
    client: &HiveRegistryClient,
    name: &str,
) -> Result<AgentSpecListing, TeamInstallError> {
    client
        .search_agents(name)
        .await?
        .items
        .into_iter()
        .find(|item| item.name == name)
        .ok_or_else(|| TeamInstallError::NotFound(name.to_string()))
}

/// Fetch and verify the team `listing` names, at `version` (its latest when
/// `None`), and download the plugins `module_dir` does not already hold.
/// Writes nothing.
pub async fn fetch_team(
    client: &HiveRegistryClient,
    registry_url: &str,
    listing: &AgentSpecListing,
    version: Option<&str>,
    module_dir: &Path,
) -> Result<FetchedTeam, TeamInstallError> {
    if listing.pricing_model != "free" {
        return Err(TeamInstallError::PaidTeam {
            name: listing.name.clone(),
            pricing: listing.pricing_model.clone(),
        });
    }
    let version = version.unwrap_or(&listing.latest_version).to_string();
    let leader = fetch_spec(client, &listing.name, &version).await?;
    let mut specs = vec![leader];
    for member in &listing.members {
        // Only a member the leader delegates to: the listing is not signed,
        // the leader's spec is.
        if specs[0].spec.swarm.delegates_to.contains(&member.name) {
            specs.push(fetch_spec(client, &member.name, &member.version).await?);
        }
    }

    let mut plugins: Vec<FetchedPlugin> = Vec::new();
    for locked in specs.iter().flat_map(|s| s.lockfile.iter()) {
        if let Some(seen) = plugins.iter().find(|p| p.locked.module == locked.module) {
            if seen.locked.version != locked.version {
                return Err(TeamInstallError::BadSpec {
                    name: listing.name.clone(),
                    version: version.clone(),
                    reason: format!(
                        "its specs lock plugin '{}' at two versions, {} and {}",
                        locked.module, seen.locked.version, locked.version
                    ),
                });
            }
            continue;
        }
        let pricing_model = listing
            .lockfile
            .iter()
            .find(|p| p.locked.module == locked.module)
            .map(|p| p.pricing_model.clone())
            .unwrap_or_else(|| "free".to_string());
        let installed = InstallRecord::read(&module_dir.join(&locked.module))
            .ok()
            .flatten()
            .is_some_and(|record| record.sha256 == locked.sha256);
        let download = if installed {
            None
        } else {
            let download =
                install::download_wasm_module(client, &locked.module, &locked.version, |_, _| {})
                    .await?;
            if download.wasm_hash != locked.sha256 {
                return Err(TeamInstallError::LockMismatch {
                    module: locked.module.clone(),
                    version: locked.version.clone(),
                    expected: locked.sha256.clone(),
                    got: download.wasm_hash.clone(),
                });
            }
            Some(download)
        };
        plugins.push(FetchedPlugin {
            locked: locked.clone(),
            pricing_model,
            download,
        });
    }

    Ok(FetchedTeam {
        leader: listing.name.clone(),
        author: listing.author_username.clone(),
        registry_url: registry_url.to_string(),
        version,
        specs,
        plugins,
    })
}

/// Fetch `name@version` and verify its chain against the client's trusted
/// roots; the spec used is the signed bytes, never the convenience copy.
async fn fetch_spec(
    client: &HiveRegistryClient,
    name: &str,
    version: &str,
) -> Result<FetchedSpec, TeamInstallError> {
    let unverified = |reason: String| TeamInstallError::Unverified {
        name: name.to_string(),
        version: version.to_string(),
        reason,
    };
    if client.root_keys().is_empty() {
        return Err(unverified(
            "this registry has no trusted root key, so nothing from it can be verified".into(),
        ));
    }
    let published = client.get_agent_spec(name, version).await?;
    let verified = verify_spec_any(client.root_keys(), &published.signed, name, version)
        .map_err(|e| unverified(e.to_string()))?;
    let bad = |reason: String| TeamInstallError::BadSpec {
        name: name.to_string(),
        version: version.to_string(),
        reason,
    };
    let spec = AgentSpec::from_json(&published.signed.spec).map_err(|e| bad(e.to_string()))?;
    spec.validate(None).map_err(|e| bad(e.to_string()))?;
    if spec.agent.name != name {
        return Err(bad(format!("it names itself '{}'", spec.agent.name)));
    }
    Ok(FetchedSpec {
        name: name.to_string(),
        version: version.to_string(),
        spec,
        lockfile: verified.manifest.lockfile,
    })
}

fn agents_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("chatty").join("agents")
}

fn records_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("chatty").join(TEAMS_RECORD_DIR)
}

/// Every installed team, by leader name.
pub fn installed_teams(data_dir: &Path) -> Vec<TeamRecord> {
    let Ok(entries) = std::fs::read_dir(records_dir(data_dir)) else {
        return Vec::new();
    };
    let mut teams: Vec<TeamRecord> = entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
        .filter_map(|e| {
            let text = std::fs::read_to_string(e.path()).ok()?;
            serde_json::from_str(&text)
                .map_err(|err| {
                    tracing::warn!(path = %e.path().display(), %err, "Skipping an unreadable team record")
                })
                .ok()
        })
        .collect();
    teams.sort_by(|a, b| a.leader.cmp(&b.leader));
    teams
}

/// Write a fetched team: its plugins into `module_dir` (and `extensions`),
/// its specs into `<data_dir>/chatty/agents/`, the leader and its delegates
/// into `roster` when that declares one, and its record. Installing a team
/// that is already installed updates it.
pub fn apply_team(
    team: FetchedTeam,
    data_dir: &Path,
    module_dir: &Path,
    extensions: &mut ExtensionsModel,
    roster: &mut Vec<String>,
) -> Result<TeamRecord, TeamInstallError> {
    let others: Vec<TeamRecord> = installed_teams(data_dir)
        .into_iter()
        .filter(|record| record.leader != team.leader)
        .collect();
    let previous = installed_teams(data_dir)
        .into_iter()
        .find(|record| record.leader == team.leader);

    // A spec name already defined by hand, or by another team, is refused
    // before anything is written.
    let agents = agents_dir(data_dir);
    for spec in &team.specs {
        let path = agents.join(format!("{}.toml", spec.name));
        let ours = previous
            .as_ref()
            .is_some_and(|record| record.specs.contains(&spec.name));
        if let Some(owner) = others.iter().find(|r| r.specs.contains(&spec.name)) {
            return Err(TeamInstallError::NameTaken {
                name: spec.name.clone(),
                path: path.display().to_string(),
                owner: format!(" (part of the installed team '{}')", owner.leader),
            });
        }
        if path.exists() && !ours {
            return Err(TeamInstallError::NameTaken {
                name: spec.name.clone(),
                path: path.display().to_string(),
                owner: String::new(),
            });
        }
    }

    // Plugins first: a spec whose plugin is missing would not load.
    let mut installed_plugins: Vec<String> = previous
        .as_ref()
        .map(|record| record.installed_plugins.clone())
        .unwrap_or_default();
    for plugin in &team.plugins {
        let module = &plugin.locked.module;
        let Some(download) = &plugin.download else {
            // Already there: a team's install is shared with this one, so
            // the last team to go removes it; the user's own install is not.
            let team_owned = others.iter().any(|r| r.installed_plugins.contains(module));
            if team_owned && !installed_plugins.contains(module) {
                installed_plugins.push(module.clone());
            }
            continue;
        };
        let was_there = extensions.is_installed(module);
        if was_there {
            install::uninstall_extension(module, module_dir, extensions)?;
        }
        install::install_wasm_module(
            download,
            module,
            &plugin.locked.version,
            module,
            &format!("Installed with the team {}", team.leader),
            &plugin.pricing_model,
            module_dir,
            extensions,
        )?;
        if !was_there && !installed_plugins.contains(module) {
            installed_plugins.push(module.clone());
        }
    }
    // A plugin a previous version installed and this one no longer locks.
    let locked: Vec<&str> = team
        .plugins
        .iter()
        .map(|p| p.locked.module.as_str())
        .collect();
    let dropped: Vec<String> = installed_plugins
        .iter()
        .filter(|m| !locked.contains(&m.as_str()))
        .cloned()
        .collect();
    installed_plugins.retain(|m| locked.contains(&m.as_str()));
    for module in dropped {
        if !others.iter().any(|r| r.uses_plugin(&module)) {
            install::uninstall_extension(&module, module_dir, extensions)?;
        }
    }

    std::fs::create_dir_all(&agents)?;
    let names: Vec<String> = team.specs.iter().map(|s| s.name.clone()).collect();
    if let Some(record) = &previous {
        for stale in record.specs.iter().filter(|name| !names.contains(name)) {
            remove_file_if_present(&agents.join(format!("{stale}.toml")))?;
        }
    }
    for spec in &team.specs {
        let header = format!(
            "# Installed from {} as part of the team {}@{} by {}.\n\
             # Signed by the publisher; uninstall the team to remove it.\n\n",
            team.registry_url, team.leader, team.version, team.author
        );
        let body = spec.spec.to_toml().map_err(|e| TeamInstallError::BadSpec {
            name: spec.name.clone(),
            version: spec.version.clone(),
            reason: e.to_string(),
        })?;
        std::fs::write(agents.join(format!("{}.toml", spec.name)), header + &body)?;
    }

    // A declared roster serves exactly what it names: add the leader and
    // every agent the team's specs delegate to by name that resolves here.
    let mut roster_added = previous
        .as_ref()
        .map(|record| record.roster_added.clone())
        .unwrap_or_default();
    if !roster.is_empty() {
        let mut wanted: BTreeSet<String> = BTreeSet::new();
        for spec in &team.specs {
            wanted.extend(
                spec.spec
                    .swarm
                    .delegates_to
                    .iter()
                    .filter(|d| !d.contains('*'))
                    .cloned(),
            );
        }
        let mut wanted: Vec<String> = wanted.into_iter().collect();
        wanted.insert(0, team.leader.clone());
        for name in wanted {
            let resolves = load_agent_spec_from(&name, None, Some(data_dir)).is_ok();
            if resolves && !roster.contains(&name) {
                roster.push(name.clone());
                roster_added.push(name);
            }
        }
    }

    let record = TeamRecord {
        leader: team.leader.clone(),
        version: team.version.clone(),
        author: team.author.clone(),
        registry_url: team.registry_url.clone(),
        specs: names,
        plugins: team
            .plugins
            .iter()
            .map(|p| format!("{}@{}", p.locked.module, p.locked.version))
            .collect(),
        paid_plugins: team.paid_plugins(),
        installed_plugins,
        roster_added,
    };
    let dir = records_dir(data_dir);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(
        dir.join(format!("{}.json", team.leader)),
        serde_json::to_string_pretty(&record).expect("a team record serialises"),
    )?;
    Ok(record)
}

fn remove_file_if_present(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Remove the team led by `leader`: its spec files, the names it added to
/// a declared `roster`, its record, and each plugin it installed that no
/// other installed team uses. A plugin that was there before it stays.
pub fn uninstall_team(
    leader: &str,
    data_dir: &Path,
    module_dir: &Path,
    extensions: &mut ExtensionsModel,
    roster: &mut Vec<String>,
) -> Result<TeamRecord, TeamInstallError> {
    let teams = installed_teams(data_dir);
    let record = teams
        .iter()
        .find(|record| record.leader == leader)
        .cloned()
        .ok_or_else(|| TeamInstallError::NotInstalled(leader.to_string()))?;
    let others: Vec<&TeamRecord> = teams.iter().filter(|r| r.leader != leader).collect();

    let agents = agents_dir(data_dir);
    for name in &record.specs {
        remove_file_if_present(&agents.join(format!("{name}.toml")))?;
    }
    roster.retain(|name| !record.roster_added.contains(name));
    for module in &record.installed_plugins {
        let still_used = others.iter().any(|r| r.uses_plugin(module));
        let is_hive_install = extensions
            .find(module)
            .is_some_and(|ext| matches!(ext.source, ExtensionSource::Hive { .. }));
        if !still_used && is_hive_install {
            install::uninstall_extension(module, module_dir, extensions)?;
        }
    }
    remove_file_if_present(&records_dir(data_dir).join(format!("{leader}.json")))?;
    Ok(record)
}

#[cfg(test)]
mod tests;
