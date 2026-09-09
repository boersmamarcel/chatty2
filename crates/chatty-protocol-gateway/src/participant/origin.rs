//! Where an agent came from (ADR-0011 C5).
//!
//! A caller that can address an agent should be able to see whose machine it
//! runs on before it hands over a prompt. The broker is the only place that
//! knows: it is what registered the participant, so it is what can say
//! whether the thing behind a name is a child process on this laptop, a
//! microVM this user leased, a URL the user typed into settings, or a card
//! learned from somewhere else.
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

    #[test]
    fn the_wire_spelling_is_the_documented_one() {
        for (origin, spelling) in [
            (AgentOrigin::Local, "local"),
            (AgentOrigin::Fleet, "fleet"),
            (AgentOrigin::RemoteConfigured, "remote_configured"),
            (AgentOrigin::Discovered, "discovered"),
        ] {
            assert_eq!(origin.as_str(), spelling);
            assert_eq!(
                serde_json::to_value(origin).unwrap(),
                serde_json::Value::String(spelling.to_string()),
                "the enum and `as_str` must agree, or the card and the tool disagree"
            );
        }
    }

    /// The reading end of this label lives in `chatty-core`
    /// (`tools::agent_origin::AgentOrigin`), which cannot depend on this
    /// crate. This is the test that stops the two vocabularies drifting: every
    /// label this crate serves has to be one that crate can read back.
    #[test]
    fn every_label_this_crate_serves_is_one_chatty_core_reads() {
        for origin in [
            AgentOrigin::Local,
            AgentOrigin::Fleet,
            AgentOrigin::RemoteConfigured,
            AgentOrigin::Discovered,
        ] {
            let read_back = chatty_core::tools::AgentOrigin::from_wire(origin.as_str())
                .unwrap_or_else(|| panic!("chatty-core cannot read {origin:?}"));
            assert_eq!(
                read_back.as_str(),
                origin.as_str(),
                "the two crates spell {origin:?} differently"
            );
            assert_eq!(
                read_back.is_own_fleet(),
                origin.is_own_fleet(),
                "the two crates disagree about whether {origin:?} is inside the fleet"
            );
        }
    }

    #[test]
    fn only_local_and_fleet_are_the_users_own() {
        assert!(AgentOrigin::Local.is_own_fleet());
        assert!(AgentOrigin::Fleet.is_own_fleet());
        assert!(!AgentOrigin::RemoteConfigured.is_own_fleet());
        assert!(!AgentOrigin::Discovered.is_own_fleet());
    }
}
