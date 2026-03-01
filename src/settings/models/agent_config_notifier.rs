use gpui::{EventEmitter, Global, WeakEntity};

/// Events that signal the active agent needs to be rebuilt.
///
/// Fired by any subsystem whose changes affect the agent's configuration:
/// MCP servers, user secrets, execution settings, etc.
#[derive(Clone, Debug)]
pub enum AgentConfigEvent {
    /// Something changed that requires the active agent to be rebuilt
    /// (e.g. MCP servers added/removed, secrets changed, execution settings updated).
    RebuildRequired,
}

/// Entity that notifies subscribers when agent-relevant configuration changes.
pub struct AgentConfigNotifier;

impl EventEmitter<AgentConfigEvent> for AgentConfigNotifier {}

impl Default for AgentConfigNotifier {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentConfigNotifier {
    pub fn new() -> Self {
        Self
    }
}

/// Global wrapper for the notifier entity.
#[derive(Default)]
pub struct GlobalAgentConfigNotifier {
    pub entity: Option<WeakEntity<AgentConfigNotifier>>,
}

impl Global for GlobalAgentConfigNotifier {}
