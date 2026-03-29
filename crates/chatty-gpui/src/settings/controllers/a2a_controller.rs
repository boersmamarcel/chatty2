use chatty_core::settings::models::a2a_store::{A2aAgentConfig, A2aAgentStatus, A2aAgentsModel};
use chatty_core::services::A2aClient;
use gpui::{App, AsyncApp};
use tracing::{error, info};

/// Create a new A2A agent entry and persist to disk.
pub fn create_agent(name: String, url: String, api_key: Option<String>, cx: &mut App) {
    let name = name.trim().to_string();
    let url = url.trim().to_string();
    let api_key = api_key.filter(|k| !k.trim().is_empty());

    // 1. Add to global state (starts enabled; connectivity is checked async)
    let updated = {
        let model = cx.global_mut::<A2aAgentsModel>();
        model.agents_mut().push(A2aAgentConfig {
            name: name.clone(),
            url: url.clone(),
            api_key,
            enabled: true,
            skills: vec![],
        });
        model.agents().to_vec()
    };

    // 2. Refresh UI immediately
    cx.refresh_windows();

    // 3. Save async to disk
    save_agents_async(updated, cx);

    info!(name = %name, url = %url, "Created new A2A agent");

    // Probe the agent card in the background (name is moved into probe)
    probe_agent_card(name, cx);
}

/// Update the API key for an existing A2A agent and persist to disk.
pub fn update_agent_api_key(agent_name: String, api_key: Option<String>, cx: &mut App) {
    let api_key = api_key.filter(|k| !k.trim().is_empty());

    let updated = {
        let model = cx.global_mut::<A2aAgentsModel>();
        if let Some(agent) = model
            .agents_mut()
            .iter_mut()
            .find(|a| a.name == agent_name)
        {
            agent.api_key = api_key;
        } else {
            error!(agent = %agent_name, "Agent not found for API key update");
            return;
        }
        model.agents().to_vec()
    };

    cx.refresh_windows();
    save_agents_async(updated, cx);

    // Re-probe with new key
    probe_agent_card(agent_name.clone(), cx);

    info!(agent = %agent_name, "Updated A2A agent API key");
}

/// Toggle the enabled state of an A2A agent and persist to disk.
pub fn toggle_agent(agent_name: String, cx: &mut App) {
    let updated = {
        let model = cx.global_mut::<A2aAgentsModel>();
        if let Some(agent) = model
            .agents_mut()
            .iter_mut()
            .find(|a| a.name == agent_name)
        {
            agent.enabled = !agent.enabled;
            info!(
                agent = %agent_name,
                enabled = agent.enabled,
                "Toggled A2A agent"
            );
        } else {
            error!(agent = %agent_name, "Agent not found for toggle");
            return;
        }
        model.agents().to_vec()
    };

    cx.refresh_windows();
    save_agents_async(updated, cx);

    // Probe only when enabling
    let is_enabled = cx
        .global::<A2aAgentsModel>()
        .agents()
        .iter()
        .find(|a| a.name == agent_name)
        .map(|a| a.enabled)
        .unwrap_or(false);

    if is_enabled {
        probe_agent_card(agent_name, cx);
    }
}

/// Delete an A2A agent by name and persist to disk.
pub fn delete_agent(agent_name: String, cx: &mut App) {
    let updated = {
        let model = cx.global_mut::<A2aAgentsModel>();
        model.agents_mut().retain(|a| a.name != agent_name);
        model.remove_status(&agent_name);
        model.agents().to_vec()
    };

    cx.refresh_windows();
    save_agents_async(updated, cx);

    info!(agent = %agent_name, "Deleted A2A agent");
}

/// Fetch the agent card for a given agent and update the status in the global model.
pub fn probe_agent_card(agent_name: String, cx: &mut App) {
    // Snapshot config before spawning
    let config = cx
        .global::<A2aAgentsModel>()
        .agents()
        .iter()
        .find(|a| a.name == agent_name)
        .cloned();

    let Some(config) = config else { return };

    // Mark as connecting immediately
    cx.global_mut::<A2aAgentsModel>()
        .set_status(agent_name.clone(), A2aAgentStatus::Connecting);
    cx.refresh_windows();

    let client = A2aClient::new();

    cx.spawn(async move |cx| {
        match client.fetch_agent_card(&config).await {
            Ok(card) => {
                let skills = card.skills.clone();
                cx.update(|cx| {
                    let model = cx.global_mut::<A2aAgentsModel>();
                    model.set_status(agent_name.clone(), A2aAgentStatus::Connected);
                    // Update cached skills
                    if let Some(agent) = model
                        .agents_mut()
                        .iter_mut()
                        .find(|a| a.name == agent_name)
                    {
                        agent.skills = skills;
                    }
                    cx.refresh_windows();
                })
                .ok();
                // Persist updated skills
                cx.update(|cx| {
                    let agents = cx.global::<A2aAgentsModel>().agents().to_vec();
                    save_agents_async(agents, cx);
                })
                .ok();
            }
            Err(e) => {
                let err_msg = format!("{e:#}");
                error!(agent = %agent_name, error = %err_msg, "Failed to fetch A2A agent card");
                cx.update(|cx| {
                    cx.global_mut::<A2aAgentsModel>()
                        .set_status(agent_name, A2aAgentStatus::Failed(err_msg));
                    cx.refresh_windows();
                })
                .ok();
            }
        }
    })
    .detach();
}

/// Save agents asynchronously to disk.
fn save_agents_async(agents: Vec<A2aAgentConfig>, cx: &mut App) {
    cx.spawn(|_cx: &mut AsyncApp| async move {
        let repo = chatty_core::a2a_repository();
        if let Err(e) = repo.save_all(agents).await {
            error!(error = ?e, "Failed to save A2A agents, changes will be lost on restart");
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_cfg(name: &str, enabled: bool) -> A2aAgentConfig {
        A2aAgentConfig {
            name: name.to_string(),
            url: format!("https://example.com/a2a/{}", name),
            api_key: None,
            enabled,
            skills: vec![],
        }
    }

    #[test]
    fn test_toggle_logic() {
        let mut agents = vec![make_cfg("agent-1", false)];
        if let Some(a) = agents.iter_mut().find(|a| a.name == "agent-1") {
            a.enabled = !a.enabled;
        }
        assert!(agents[0].enabled);
    }

    #[test]
    fn test_delete_logic() {
        let mut agents = vec![make_cfg("a", true), make_cfg("b", false)];
        agents.retain(|a| a.name != "a");
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].name, "b");
    }
}
