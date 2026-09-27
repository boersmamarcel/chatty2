//! Where an agent came from (ADR-0011 C5).
//!
//! A caller that can address an agent should be able to see whose machine it
//! runs on before it hands over a prompt. The broker is the only place that
//! knows: it is what registered the participant, so it is what can say
//! whether the thing behind a name is a child process on this laptop, a
//! microVM this user leased, a URL the user typed into settings, or a card
//! learned from somewhere else. The gateway serves the label on the agent
//! card; `list_agents` and `invoke_agent` in chatty-core read it back.
//!
//! This is a property of the *registration*, not of the card. A participant
//! describes itself in its card; it does not get to describe its own
//! provenance, or the label would be worth nothing.

use serde::{Deserialize, Serialize};

/// Whose machine an agent runs on, from the point of view of the user whose
/// broker is answering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentOrigin {
    /// A process on this machine: a spawned worker, a WASM module, the
    /// runner's own virtual agent.
    Local,
    /// Somewhere else in this user's own fleet — a leased microVM registering
    /// over vsock (AGE-307), not a third party.
    Fleet,
    /// A URL the user configured in Settings → A2A Agents. Outside the fleet,
    /// but chosen deliberately.
    RemoteConfigured,
    /// Learned from another agent's card rather than configured. Outside the
    /// fleet and not chosen by anyone — the case that most needs saying out
    /// loud. Nothing publishes this yet; A2A has no peer-discovery hop in
    /// chatty today, and the label exists so that the hop cannot be added
    /// without deciding what it means.
    Discovered,
}

impl AgentOrigin {
    /// The wire spelling, which is also what `list_agents` shows the model.
    pub fn as_str(&self) -> &'static str {
        match self {
            AgentOrigin::Local => "local",
            AgentOrigin::Fleet => "fleet",
            AgentOrigin::RemoteConfigured => "remote_configured",
            AgentOrigin::Discovered => "discovered",
        }
    }

    /// Read a label off an agent card.
    ///
    /// `None` for anything this build does not know, which callers treat as
    /// outside the fleet — an unknown provenance is not a reason to trust
    /// something.
    pub fn from_wire(label: &str) -> Option<Self> {
        match label {
            "local" => Some(AgentOrigin::Local),
            "fleet" => Some(AgentOrigin::Fleet),
            "remote_configured" => Some(AgentOrigin::RemoteConfigured),
            "discovered" => Some(AgentOrigin::Discovered),
            _ => None,
        }
    }

    /// Whether this origin is inside the user's own fleet.
    ///
    /// The question `invoke_agent` asks before handing over a prompt: a
    /// worker on this machine or in a microVM the user leased is theirs, and
    /// anything else is a third party however it got into the list.
    pub fn is_own_fleet(&self) -> bool {
        matches!(self, AgentOrigin::Local | AgentOrigin::Fleet)
    }
}

impl std::fmt::Display for AgentOrigin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [AgentOrigin; 4] = [
        AgentOrigin::Local,
        AgentOrigin::Fleet,
        AgentOrigin::RemoteConfigured,
        AgentOrigin::Discovered,
    ];

    #[test]
    fn the_wire_spelling_is_the_documented_one() {
        for (origin, spelling) in
            ALL.into_iter()
                .zip(["local", "fleet", "remote_configured", "discovered"])
        {
            assert_eq!(origin.as_str(), spelling);
            assert_eq!(
                serde_json::to_value(origin).unwrap(),
                serde_json::Value::String(spelling.to_string()),
                "the enum and `as_str` must agree, or the card and the tool disagree"
            );
        }
    }

    #[test]
    fn every_label_round_trips() {
        for origin in ALL {
            assert_eq!(AgentOrigin::from_wire(origin.as_str()), Some(origin));
        }
    }

    #[test]
    fn an_unknown_label_is_not_a_fleet_member() {
        assert_eq!(AgentOrigin::from_wire("something_new"), None);
        assert!(AgentOrigin::Local.is_own_fleet());
        assert!(AgentOrigin::Fleet.is_own_fleet());
        assert!(!AgentOrigin::RemoteConfigured.is_own_fleet());
        assert!(!AgentOrigin::Discovered.is_own_fleet());
    }
}
