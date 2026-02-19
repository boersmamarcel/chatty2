use gpui::{EventEmitter, Global, WeakEntity};

/// Events related to MCP server configuration changes
#[derive(Clone, Debug)]
pub enum McpNotifierEvent {
    /// Emitted when a new MCP server is added
    ServerAdded,
}

/// Entity that notifies subscribers when MCP server configs change
pub struct McpNotifier;

impl EventEmitter<McpNotifierEvent> for McpNotifier {}

impl McpNotifier {
    pub fn new() -> Self {
        Self
    }
}

/// Global wrapper for the notifier entity
#[derive(Default)]
pub struct GlobalMcpNotifier {
    pub entity: Option<WeakEntity<McpNotifier>>,
}

impl Global for GlobalMcpNotifier {}
