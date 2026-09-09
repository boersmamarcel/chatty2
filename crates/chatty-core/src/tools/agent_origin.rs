//! Whose machine an agent runs on, as `list_agents` reports it and
//! `invoke_agent` reads it (ADR-0011 C5).
//!
//! The broker decides the label — it is what registered the participant — and
//! serves it on the aggregated agent card. This is the reading end of that
//! seam. The vocabulary is duplicated rather than shared because
//! `chatty-protocol-gateway` is not a dependency of this crate; the gateway
//! has *this* crate as a dev-dependency, and a test there pins the two
//! spellings against each other so the seam cannot drift silently.

use serde::{Deserialize, Serialize};

/// Where an agent came from, from the point of view of this user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentOrigin {
    /// A process on this machine: a spawned worker, a WASM module.
    Local,
    /// Elsewhere in this user's own fleet — a microVM they leased.
    Fleet,
    /// A URL configured in Settings → A2A Agents: outside the fleet, but
    /// chosen deliberately.
    RemoteConfigured,
    /// Learned from another agent's card rather than configured. Outside the
    /// fleet and chosen by nobody.
    Discovered,
}

impl AgentOrigin {
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
    /// `None` for anything this build does not know, which is treated as
    /// outside the fleet by [`is_own_fleet`](Self::is_own_fleet)'s callers —
    /// an unknown provenance is not a reason to trust something.
    pub fn from_wire(label: &str) -> Option<Self> {
        match label {
            "local" => Some(AgentOrigin::Local),
            "fleet" => Some(AgentOrigin::Fleet),
            "remote_configured" => Some(AgentOrigin::RemoteConfigured),
            "discovered" => Some(AgentOrigin::Discovered),
            _ => None,
        }
    }

    /// Whether the agent runs on hardware this user controls.
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
    fn every_label_round_trips() {
        for origin in [
            AgentOrigin::Local,
            AgentOrigin::Fleet,
            AgentOrigin::RemoteConfigured,
            AgentOrigin::Discovered,
        ] {
            assert_eq!(AgentOrigin::from_wire(origin.as_str()), Some(origin));
        }
    }

    #[test]
    fn an_unknown_label_is_not_a_fleet_member() {
        assert_eq!(AgentOrigin::from_wire("something_new"), None);
        assert!(!AgentOrigin::RemoteConfigured.is_own_fleet());
        assert!(!AgentOrigin::Discovered.is_own_fleet());
        assert!(AgentOrigin::Local.is_own_fleet());
        assert!(AgentOrigin::Fleet.is_own_fleet());
    }
}
