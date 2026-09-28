use chatty_core::services::lazy_broker::LazyBroker;
use chatty_module_registry::TrustLevel;
use chatty_protocol_gateway::ProtocolGateway;
use gpui::Global;
use std::sync::Arc;

#[derive(Clone, Debug)]
pub enum ModuleLoadStatus {
    Loaded,
    /// The registry's scan refused the module, with its reason.
    Error(String),
    /// Module runs remotely on hive-runner; no local WASM binary.
    Remote,
}

#[allow(dead_code)]
#[derive(Clone, Debug)]
pub struct DiscoveredModuleEntry {
    pub directory_name: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub wasm_file: String,
    pub tools: Vec<String>,
    pub agent: bool,
    pub mcp: bool,
    pub a2a: bool,
    pub status: ModuleLoadStatus,
    /// `"local"`, `"remote"`, or `"remote_only"`.
    pub execution_mode: String,
    /// The trust a loaded module loaded at (PL-H5a): its install record's,
    /// or `Local` for one copied in by hand. `None` unless it loaded.
    pub trust_level: Option<TrustLevel>,
}

pub struct DiscoveredModulesModel {
    pub modules: Vec<DiscoveredModuleEntry>,
    pub scan_error: Option<String>,
    pub gateway_status: String,
    pub last_scanned_dir: String,
    pub scanning: bool,
    pub refresh_generation: u64,
    pub gateway: Option<ProtocolGateway>,
    /// The gateway/broker, captured to start on the first
    /// `list_agents`/`invoke_agent` call instead of at boot (BI-2,
    /// AGE-634). `None` until `refresh_runtime` publishes one (the module
    /// gateway setting is on), and replaced outright on every later
    /// refresh.
    pub lazy_broker: Option<Arc<dyn LazyBroker>>,
}

impl Default for DiscoveredModulesModel {
    fn default() -> Self {
        Self {
            modules: Vec::new(),
            scan_error: None,
            gateway_status: "Module runtime disabled".to_string(),
            last_scanned_dir: String::new(),
            scanning: false,
            refresh_generation: 0,
            gateway: None,
            lazy_broker: None,
        }
    }
}

impl Global for DiscoveredModulesModel {}
