use serde::{Deserialize, Serialize};

/// Configuration for a single remote A2A agent.
///
/// An A2A agent is a remote HTTP service that implements the Agent-to-Agent
/// (A2A) protocol: it exposes an agent card at `/.well-known/agent.json` and
/// accepts `message/send` JSON-RPC requests at its base URL.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct A2aAgentConfig {
    /// User-visible name (also used as the first word of `/agent <name> <prompt>`).
    pub name: String,

    /// Base URL of the remote A2A agent endpoint
    /// (e.g. `https://hive.dev/a2a/voucher-agent`).
    pub url: String,

    /// Optional Bearer token sent as `Authorization: Bearer <api_key>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,

    /// Whether this agent is enabled/active.
    #[serde(default = "default_enabled")]
    pub enabled: bool,

    /// Skills discovered from the remote agent card (cached, not always present).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<String>,
}

fn default_enabled() -> bool {
    true
}

impl A2aAgentConfig {
    /// Returns true if an API key has been configured.
    pub fn has_api_key(&self) -> bool {
        self.api_key.as_deref().is_some_and(|k| !k.is_empty())
    }
}

/// Connection status for a remote A2A agent (not persisted).
#[derive(Clone, Debug, PartialEq)]
pub enum A2aAgentStatus {
    /// Not yet checked or not relevant.
    Unknown,
    /// Agent card was successfully fetched; agent is reachable.
    Connected,
    /// Fetching agent card or testing connectivity.
    Connecting,
    /// Agent card fetch failed.
    Failed(String),
}

/// Global store for A2A agent configurations.
#[derive(Clone)]
pub struct A2aAgentsModel {
    agents: Vec<A2aAgentConfig>,
    /// Runtime connection status per agent (not persisted).
    statuses: std::collections::HashMap<String, A2aAgentStatus>,
}

impl A2aAgentsModel {
    pub fn new() -> Self {
        Self {
            agents: Vec::new(),
            statuses: std::collections::HashMap::new(),
        }
    }

    pub fn agents(&self) -> &[A2aAgentConfig] {
        &self.agents
    }

    /// Mutable access to the agent list (for in-place updates).
    pub fn agents_mut(&mut self) -> &mut Vec<A2aAgentConfig> {
        &mut self.agents
    }

    /// Replace the entire list (used when loading from disk).
    pub fn replace_all(&mut self, agents: Vec<A2aAgentConfig>) {
        self.agents = agents;
    }

    /// Count enabled agents.
    pub fn enabled_count(&self) -> usize {
        self.agents.iter().filter(|a| a.enabled).count()
    }

    /// Look up an enabled agent by name.
    pub fn find_enabled(&self, name: &str) -> Option<&A2aAgentConfig> {
        self.agents.iter().find(|a| a.enabled && a.name == name)
    }

    /// Get the connection status for a given agent.
    pub fn status(&self, agent_name: &str) -> &A2aAgentStatus {
        self.statuses
            .get(agent_name)
            .unwrap_or(&A2aAgentStatus::Unknown)
    }

    /// Set the connection status for a given agent.
    pub fn set_status(&mut self, agent_name: String, status: A2aAgentStatus) {
        self.statuses.insert(agent_name, status);
    }

    /// Remove the status entry for a given agent (e.g. on delete).
    pub fn remove_status(&mut self, agent_name: &str) {
        self.statuses.remove(agent_name);
    }
}

impl Default for A2aAgentsModel {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_agent(name: &str, enabled: bool) -> A2aAgentConfig {
        A2aAgentConfig {
            name: name.to_string(),
            url: format!("https://example.com/a2a/{}", name),
            api_key: None,
            enabled,
            skills: vec![],
        }
    }

    #[test]
    fn test_serialization_round_trip() {
        let cfg = make_agent("voucher-agent", true);
        let json = serde_json::to_string(&cfg).unwrap();
        let back: A2aAgentConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.name, cfg.name);
        assert_eq!(back.url, cfg.url);
        assert!(back.enabled);
        assert!(!json.contains("api_key"));
    }

    #[test]
    fn test_api_key_serialization() {
        let mut cfg = make_agent("test", true);
        cfg.api_key = Some("secret".to_string());
        let json = serde_json::to_string(&cfg).unwrap();
        assert!(json.contains("\"api_key\":\"secret\""));
        let back: A2aAgentConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.api_key.as_deref(), Some("secret"));
    }

    #[test]
    fn test_default_enabled_via_deserialization() {
        let json = r#"{"name":"a","url":"https://example.com"}"#;
        let cfg: A2aAgentConfig = serde_json::from_str(json).unwrap();
        assert!(cfg.enabled);
    }

    #[test]
    fn test_has_api_key() {
        let mut cfg = make_agent("a", true);
        assert!(!cfg.has_api_key());
        cfg.api_key = Some("".to_string());
        assert!(!cfg.has_api_key());
        cfg.api_key = Some("tok".to_string());
        assert!(cfg.has_api_key());
    }

    #[test]
    fn test_model_enabled_count() {
        let mut model = A2aAgentsModel::new();
        model.replace_all(vec![
            make_agent("a", true),
            make_agent("b", false),
            make_agent("c", true),
        ]);
        assert_eq!(model.enabled_count(), 2);
    }

    #[test]
    fn test_find_enabled() {
        let mut model = A2aAgentsModel::new();
        model.replace_all(vec![make_agent("x", true), make_agent("y", false)]);
        assert!(model.find_enabled("x").is_some());
        assert!(model.find_enabled("y").is_none());
        assert!(model.find_enabled("z").is_none());
    }

    #[test]
    fn test_status_lifecycle() {
        let mut model = A2aAgentsModel::new();
        assert_eq!(model.status("a"), &A2aAgentStatus::Unknown);
        model.set_status("a".to_string(), A2aAgentStatus::Connected);
        assert_eq!(model.status("a"), &A2aAgentStatus::Connected);
        model.remove_status("a");
        assert_eq!(model.status("a"), &A2aAgentStatus::Unknown);
    }
}
