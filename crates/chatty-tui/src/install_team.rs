//! `chatty-tui --install-team name[@version]` (MK-T1, AGE-841): install a
//! published team from the Hive marketplace the way the desktop's
//! **Install team** does — every spec and locked plugin verified — then exit.

use anyhow::{Context, Result, bail};
use chatty_core::hive::{HiveRegistryClient, HiveSession, SessionState};
use chatty_core::team_install;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub async fn run(team_ref: &str) -> Result<()> {
    let (name, version) = team_install::parse_team_ref(team_ref);
    if name.is_empty() {
        bail!("--install-team needs a team name, e.g. --install-team payments-lead@1.0.0");
    }
    let data_dir = dirs::data_dir().context("no platform data directory (is HOME set?)")?;

    let (hive, extensions, module_settings) = tokio::join!(
        chatty_core::hive_settings_repository().load(),
        chatty_core::extensions_repository().load(),
        chatty_core::module_settings_repository().load(),
    );
    let mut hive = hive.unwrap_or_default();
    let mut extensions = extensions.unwrap_or_default();
    let mut module_settings = module_settings.context("Failed to load module settings")?;
    let Some(pair) = hive.token_pair() else {
        bail!(
            "sign in to Hive first (Settings → Extensions in the desktop): \
             fetching a team's specs needs your account"
        );
    };

    let session = Arc::new(HiveSession::new(&hive.registry_url, Some(pair.clone())));
    let client = HiveRegistryClient::new(&hive.registry_url).with_session(session.clone());
    let module_dir = PathBuf::from(&module_settings.module_dir);

    let fetched = async {
        let listing = team_install::find_listing(&client, &name).await?;
        team_install::fetch_team(
            &client,
            &hive.registry_url,
            &listing,
            version.as_deref(),
            &module_dir,
        )
        .await
    }
    .await;
    // A refresh during the fetch rotated the pair; the old refresh token is
    // spent, so keep the new one whatever happened next.
    let state = session.subscribe().borrow().clone();
    if let SessionState::SignedIn(rotated) = state
        && rotated != pair
    {
        hive.set_token_pair(Some(&rotated));
        chatty_core::hive_settings_repository()
            .save(hive.clone())
            .await
            .context("Failed to save the refreshed Hive sign-in")?;
    }
    let team = fetched?;

    let record = team_install::apply_team(
        team,
        &data_dir,
        &module_dir,
        &mut extensions,
        &mut module_settings.virtual_agents,
    )?;
    chatty_core::extensions_repository()
        .save(extensions)
        .await
        .context("Failed to save extensions")?;
    if !record.roster_added.is_empty() {
        chatty_core::module_settings_repository()
            .save(module_settings)
            .await
            .context("Failed to save module settings")?;
    }
    if let Err(e) = client
        .record_agent_install(&record.leader, &record.version)
        .await
    {
        tracing::warn!(error = %e, "Could not count the install");
    }

    print!("{}", summary(&record, &data_dir));
    Ok(())
}

/// What `--install-team` prints once the team is installed.
fn summary(record: &team_install::TeamRecord, data_dir: &Path) -> String {
    let mut out = format!(
        "Installed {}@{} by {} (signature verified).\n  agents: {} in {}\n",
        record.leader,
        record.version,
        record.author,
        record.specs.join(", "),
        data_dir.join("chatty").join("agents").display(),
    );
    if !record.plugins.is_empty() {
        out += &format!("  plugins (locked): {}\n", record.plugins.join(", "));
    }
    for module in &record.paid_plugins {
        out += &format!(
            "  {module} is paid: its calls need credits and its billing grant (Settings → Plugins).\n"
        );
    }
    out += &format!(
        "Run it: chatty-tui --broker, then `/agent {} <task>`.\n",
        record.leader
    );
    out
}
