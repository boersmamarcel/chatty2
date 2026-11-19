use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::a2a_store::{A2aAgentConfig, A2aAgentStatus};
use super::mcp_store::{McpAuthStatus, McpServerConfig};

// ── Extension source ───────────────────────────────────────────────────────

/// Where this extension was installed from.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ExtensionSource {
    /// Installed from the Hive marketplace.
    #[serde(rename = "hive")]
    Hive {
        module_name: String,
        version: String,
    },
    /// Manually configured by the user.
    #[serde(rename = "custom")]
    Custom,
}

// ── Extension kind ─────────────────────────────────────────────────────────

/// The technical type of the extension — determines how it is activated.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum ExtensionKind {
    /// A remote MCP server (stdio or SSE).
    #[serde(rename = "mcp")]
    McpServer(McpServerConfig),
    /// A local WASM module (loaded by the module runtime).
    #[serde(rename = "wasm")]
    WasmModule,
    /// A remote A2A agent endpoint.
    #[serde(rename = "a2a")]
    A2aAgent(A2aAgentConfig),
}

// ── Installed extension ────────────────────────────────────────────────────

/// A single installed extension, regardless of its technical type.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InstalledExtension {
    /// Unique slug (e.g. `"github-mcp"` or `"readability-auditor"`).
    pub id: String,
    /// Human-readable name shown in the UI.
    pub display_name: String,
    /// Short description.
    pub description: String,
    /// The technical backing of this extension.
    pub kind: ExtensionKind,
    /// Where this extension came from.
    pub source: ExtensionSource,
    /// Marketplace pricing classification for Hive-sourced modules.
    #[serde(default)]
    pub pricing_model: Option<String>,
    /// Whether the extension is currently active.
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

// ── Store ──────────────────────────────────────────────────────────────────

/// Global store for all installed extensions (MCP, WASM, A2A).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ExtensionsModel {
    #[serde(default)]
    pub extensions: Vec<InstalledExtension>,

    /// Runtime auth status per MCP server name (not persisted).
    #[serde(skip)]
    mcp_auth_statuses: HashMap<String, McpAuthStatus>,

    /// Runtime connection status per A2A agent name (not persisted).
    #[serde(skip)]
    a2a_statuses: HashMap<String, A2aAgentStatus>,
}

impl ExtensionsModel {
    /// The module names of the installed Hive WASM modules priced `paid`:
    /// the modules a [`ModuleMeter`](hive_client::ModuleMeter) bills.
    pub fn paid_wasm_modules(&self) -> std::collections::HashSet<String> {
        self.extensions
            .iter()
            .filter(|e| {
                matches!(e.kind, ExtensionKind::WasmModule)
                    && e.pricing_model.as_deref() == Some("paid")
            })
            .filter_map(|e| match &e.source {
                ExtensionSource::Hive { module_name, .. } => Some(module_name.clone()),
                _ => None,
            })
            .collect()
    }

    /// Return all enabled MCP server configs (borrowed).
    pub fn mcp_servers(&self) -> Vec<&McpServerConfig> {
        self.extensions
            .iter()
            .filter(|e| e.enabled)
            .filter_map(|e| match &e.kind {
                ExtensionKind::McpServer(cfg) => Some(cfg),
                _ => None,
            })
            .collect()
    }

    /// Return all enabled A2A agent configs (borrowed).
    pub fn a2a_agents(&self) -> Vec<&A2aAgentConfig> {
        self.extensions
            .iter()
            .filter(|e| e.enabled)
            .filter_map(|e| match &e.kind {
                ExtensionKind::A2aAgent(cfg) => Some(cfg),
                _ => None,
            })
            .collect()
    }

    pub fn find(&self, id: &str) -> Option<&InstalledExtension> {
        self.extensions.iter().find(|e| e.id == id)
    }

    pub fn find_mut(&mut self, id: &str) -> Option<&mut InstalledExtension> {
        self.extensions.iter_mut().find(|e| e.id == id)
    }

    pub fn is_installed(&self, id: &str) -> bool {
        self.extensions.iter().any(|e| e.id == id)
    }

    pub fn add(&mut self, ext: InstalledExtension) {
        if !self.is_installed(&ext.id) {
            self.extensions.push(ext);
        }
    }

    pub fn remove(&mut self, id: &str) {
        self.extensions.retain(|e| e.id != id);
    }

    // ── MCP convenience methods ────────────────────────────────────────────

    /// Return all MCP server configs (regardless of enabled state).
    pub fn all_mcp_servers(&self) -> Vec<(String, McpServerConfig, bool)> {
        self.extensions
            .iter()
            .filter_map(|e| match &e.kind {
                ExtensionKind::McpServer(cfg) => Some((e.id.clone(), cfg.clone(), e.enabled)),
                _ => None,
            })
            .collect()
    }

    /// Every MCP server config, in order, with `enabled` taken from the
    /// extension's flag. This is the list persisted to `mcp_servers.json` and
    /// handed to the MCP service.
    pub fn mcp_server_configs(&self) -> Vec<McpServerConfig> {
        self.extensions
            .iter()
            .filter_map(|e| match &e.kind {
                ExtensionKind::McpServer(cfg) => Some(McpServerConfig {
                    enabled: e.enabled,
                    ..cfg.clone()
                }),
                _ => None,
            })
            .collect()
    }

    /// Apply a server list pushed over `MCP_UPDATE_SENDER`: add new servers
    /// and update known ones (matched by `mcp-<name>` id). Never removes
    /// anything; non-MCP extensions are untouched.
    pub fn apply_mcp_update(&mut self, servers: &[McpServerConfig]) {
        for server in servers {
            let ext_id = format!("mcp-{}", server.name);
            if let Some(ext) = self.find_mut(&ext_id) {
                ext.enabled = server.enabled;
                ext.kind = ExtensionKind::McpServer(server.clone());
            } else {
                self.add(InstalledExtension {
                    id: ext_id,
                    display_name: server.name.clone(),
                    description: String::new(),
                    kind: ExtensionKind::McpServer(server.clone()),
                    source: ExtensionSource::Custom,
                    pricing_model: None,
                    enabled: server.enabled,
                });
            }
        }
    }

    /// [`Self::mcp_server_configs`] for persisting to `mcp_servers.json`, with
    /// the Hive token injected into the "hive" entry. The token lives only in
    /// that file and in memory-only copies: it is never written into this
    /// model, so `extensions.json` can not leak it.
    pub fn mcp_server_configs_with_hive_token(&self, token: Option<&str>) -> Vec<McpServerConfig> {
        let mut servers = self.mcp_server_configs();
        if let Some(token) = token
            && let Some(hive) = servers.iter_mut().find(|s| s.name == "hive")
        {
            hive.api_key = Some(token.to_string());
        }
        servers
    }

    /// The runtime auth status of an MCP server, if one was recorded.
    pub fn mcp_auth_status(&self, server_name: &str) -> Option<&McpAuthStatus> {
        self.mcp_auth_statuses.get(server_name)
    }

    /// Count of enabled MCP server extensions.
    pub fn enabled_mcp_count(&self) -> usize {
        self.extensions
            .iter()
            .filter(|e| e.enabled && matches!(e.kind, ExtensionKind::McpServer(_)))
            .count()
    }

    /// Set the runtime auth status for an MCP server.
    pub fn set_mcp_auth_status(&mut self, server_name: String, status: McpAuthStatus) {
        self.mcp_auth_statuses.insert(server_name, status);
    }

    // ── A2A convenience methods ────────────────────────────────────────────

    /// Return all A2A agent configs (regardless of enabled state).
    pub fn all_a2a_agents(&self) -> Vec<(String, A2aAgentConfig, bool)> {
        self.extensions
            .iter()
            .filter_map(|e| match &e.kind {
                ExtensionKind::A2aAgent(cfg) => Some((e.id.clone(), cfg.clone(), e.enabled)),
                _ => None,
            })
            .collect()
    }

    /// Return just the A2A agent configs (for agent_factory compatibility).
    pub fn a2a_agent_configs(&self) -> Vec<A2aAgentConfig> {
        self.extensions
            .iter()
            .filter_map(|e| match &e.kind {
                ExtensionKind::A2aAgent(cfg) => Some(cfg.clone()),
                _ => None,
            })
            .collect()
    }

    /// Find an A2A extension by its agent name.
    pub fn find_a2a_by_name(&self, name: &str) -> Option<&InstalledExtension> {
        self.extensions.iter().find(|e| match &e.kind {
            ExtensionKind::A2aAgent(cfg) => cfg.name == name,
            _ => false,
        })
    }

    /// Look up an enabled A2A agent config by name.
    pub fn find_enabled_a2a(&self, name: &str) -> Option<&A2aAgentConfig> {
        self.extensions.iter().find_map(|e| match &e.kind {
            ExtensionKind::A2aAgent(cfg) if e.enabled && cfg.name == name => Some(cfg),
            _ => None,
        })
    }

    /// Count of enabled A2A agent extensions.
    pub fn enabled_a2a_count(&self) -> usize {
        self.extensions
            .iter()
            .filter(|e| e.enabled && matches!(e.kind, ExtensionKind::A2aAgent(_)))
            .count()
    }

    /// Set the runtime connection status for an A2A agent.
    pub fn set_a2a_status(&mut self, agent_name: String, status: A2aAgentStatus) {
        self.a2a_statuses.insert(agent_name, status);
    }

    // ── Generic helpers ────────────────────────────────────────────────────

    /// Toggle the enabled flag on an extension. Returns the new enabled state,
    /// or `None` if the extension was not found.
    pub fn toggle_extension(&mut self, id: &str) -> Option<bool> {
        if let Some(ext) = self.find_mut(id) {
            ext.enabled = !ext.enabled;
            Some(ext.enabled)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod mcp_config_tests {
    use super::*;

    fn mcp(name: &str, ext_enabled: bool, cfg_enabled: bool) -> InstalledExtension {
        InstalledExtension {
            id: format!("mcp-{name}"),
            display_name: name.to_string(),
            description: String::new(),
            kind: ExtensionKind::McpServer(McpServerConfig {
                name: name.to_string(),
                url: format!("http://localhost/{name}"),
                api_key: None,
                enabled: cfg_enabled,
                is_module: false,
            }),
            source: ExtensionSource::Custom,
            pricing_model: None,
            enabled: ext_enabled,
        }
    }

    #[test]
    fn mcp_server_configs_keeps_order_and_uses_extension_flag() {
        let mut model = ExtensionsModel::default();
        model.add(mcp("a", true, false));
        model.add(InstalledExtension {
            kind: ExtensionKind::WasmModule,
            ..mcp("wasm", true, true)
        });
        model.add(mcp("b", false, true));

        let configs = model.mcp_server_configs();
        let names: Vec<_> = configs.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["a", "b"]);
        assert!(configs[0].enabled);
        assert!(!configs[1].enabled);
    }

    #[test]
    fn apply_mcp_update_adds_and_updates_but_never_removes() {
        let mut model = ExtensionsModel::default();
        model.add(mcp("keep", true, true));
        model.add(mcp("unlisted", true, true));
        model.add(InstalledExtension {
            kind: ExtensionKind::WasmModule,
            ..mcp("wasm", true, true)
        });

        let cfg = |name: &str, enabled: bool| McpServerConfig {
            name: name.to_string(),
            url: format!("http://localhost/new-{name}"),
            api_key: Some("k".to_string()),
            enabled,
            is_module: false,
        };
        model.apply_mcp_update(&[cfg("keep", false), cfg("fresh", true)]);

        assert!(model.find("mcp-unlisted").is_some());
        assert!(model.find("mcp-wasm").is_some());
        let keep = model.find("mcp-keep").unwrap();
        assert!(!keep.enabled);
        assert!(matches!(&keep.kind, ExtensionKind::McpServer(c) if c.url.ends_with("new-keep")));
        assert!(model.find("mcp-fresh").unwrap().enabled);
    }

    #[test]
    fn hive_token_is_injected_into_persisted_list_only() {
        let mut model = ExtensionsModel::default();
        model.add(mcp("hive", true, true));
        model.add(mcp("other", true, true));

        let persisted = model.mcp_server_configs_with_hive_token(Some("jwt-secret"));
        assert_eq!(persisted[0].api_key.as_deref(), Some("jwt-secret"));
        assert_eq!(persisted[1].api_key, None);

        // The model itself (what extensions.json is written from) is clean.
        let json = serde_json::to_string(&model).unwrap();
        assert!(!json.contains("jwt-secret"));
        assert!(!json.contains("api_key"));

        // No token: the list is the plain derivation.
        assert_eq!(
            model.mcp_server_configs_with_hive_token(None),
            model.mcp_server_configs()
        );
    }
}
