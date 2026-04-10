use chatty_core::services::A2aClient;
use chatty_core::settings::models::a2a_store::A2aAgentStatus;
use chatty_core::settings::models::extensions_store::{ExtensionKind, ExtensionsModel};
use gpui::{App, AsyncApp};
use tracing::error;

/// Fetch the agent card for a given agent and update the status in the global model.
pub fn probe_agent_card(ext_id: String, agent_name: String, cx: &mut App) {
    let config = cx
        .global::<ExtensionsModel>()
        .find_a2a_by_name(&agent_name)
        .and_then(|ext| match &ext.kind {
            ExtensionKind::A2aAgent(cfg) => Some(cfg.clone()),
            _ => None,
        });

    let Some(config) = config else { return };

    cx.global_mut::<ExtensionsModel>()
        .set_a2a_status(agent_name.clone(), A2aAgentStatus::Connecting);
    cx.refresh_windows();

    let client = A2aClient::new();

    cx.spawn(async move |cx| {
        match client.fetch_agent_card(&config).await {
            Ok(card) => {
                let skills = card.skills.clone();
                cx.update(|cx| {
                    let model = cx.global_mut::<ExtensionsModel>();
                    model.set_a2a_status(agent_name.clone(), A2aAgentStatus::Connected);
                    if let Some(ext) = model.find_mut(&ext_id)
                        && let ExtensionKind::A2aAgent(ref mut cfg) = ext.kind
                    {
                        cfg.skills = skills;
                    }
                    cx.refresh_windows();
                })
                .ok();
                // Persist updated skills
                cx.update(|cx| {
                    let ext_model = cx.global::<ExtensionsModel>().clone();
                    save_extensions_async(ext_model, cx);
                })
                .ok();
            }
            Err(e) => {
                let err_msg = format!("{e:#}");
                error!(agent = %agent_name, error = %err_msg, "Failed to fetch A2A agent card");
                cx.update(|cx| {
                    cx.global_mut::<ExtensionsModel>()
                        .set_a2a_status(agent_name.clone(), A2aAgentStatus::Failed(err_msg));
                    cx.refresh_windows();
                })
                .ok();
            }
        }
    })
    .detach();
}

fn save_extensions_async(model: ExtensionsModel, cx: &mut App) {
    cx.spawn(|_cx: &mut AsyncApp| async move {
        let repo = chatty_core::extensions_repository();
        if let Err(e) = repo.save(model).await {
            error!(error = ?e, "Failed to save extensions");
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use chatty_core::settings::models::a2a_store::A2aAgentConfig;

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
