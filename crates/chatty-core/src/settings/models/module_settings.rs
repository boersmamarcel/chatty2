use serde::{Deserialize, Serialize};

/// Settings for the WASM module runtime and protocol gateway.
#[derive(Clone, Serialize, Deserialize)]
pub struct ModuleSettingsModel {
    /// Whether the module runtime is enabled. Defaults to `false`.
    #[serde(default)]
    pub enabled: bool,
    /// Directory to scan for WASM modules.
    /// Defaults to `.chatty/modules` (relative to the working directory).
    #[serde(default = "default_module_dir")]
    pub module_dir: String,
    /// TCP port for the local protocol gateway.
    /// Defaults to `8420`.
    #[serde(default = "default_gateway_port")]
    pub gateway_port: u16,
}

fn default_module_dir() -> String {
    ".chatty/modules".to_string()
}

fn default_gateway_port() -> u16 {
    8420
}

impl Default for ModuleSettingsModel {
    fn default() -> Self {
        Self {
            enabled: false,
            module_dir: default_module_dir(),
            gateway_port: default_gateway_port(),
        }
    }
}
