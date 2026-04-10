use chatty_protocol_gateway::ProtocolGateway;
use gpui::Global;

#[derive(Clone, Debug)]
pub enum ModuleLoadStatus {
    Loaded,
    #[allow(dead_code)]
    Error(String),
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
    pub chat: bool,
    pub agent: bool,
    pub openai_compat: bool,
    pub mcp: bool,
    pub a2a: bool,
    pub status: ModuleLoadStatus,
}

pub struct DiscoveredModulesModel {
    pub modules: Vec<DiscoveredModuleEntry>,
    pub scan_error: Option<String>,
    pub gateway_status: String,
    pub last_scanned_dir: String,
    pub scanning: bool,
    pub refresh_generation: u64,
    pub gateway: Option<ProtocolGateway>,
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
        }
    }
}

impl Global for DiscoveredModulesModel {}
