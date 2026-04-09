//! One-time migration from legacy per-type config files to the unified
//! `ExtensionsModel`.
//!
//! On first launch with the new code, if `extensions.json` does not yet exist,
//! this module reads `mcp_servers.json` and `a2a_agents.json` and converts
//! every entry into an `InstalledExtension` with `ExtensionSource::Custom`.
//! The old files are left in place (read-only fallback) but are no longer
//! written to by the new code path.

use std::path::Path;

use crate::settings::models::a2a_store::A2aAgentConfig;
use crate::settings::models::extensions_store::{
    ExtensionKind, ExtensionSource, ExtensionsModel, InstalledExtension,
};
use crate::settings::models::mcp_store::McpServerConfig;

/// Attempt to migrate legacy MCP and A2A configs into the given
/// `ExtensionsModel`. Returns `true` if any entries were migrated.
pub fn migrate_legacy_configs(extensions: &mut ExtensionsModel) -> bool {
    let config_dir = match dirs::config_dir() {
        Some(d) => d.join("chatty"),
        None => return false,
    };

    let mut migrated = false;
    migrated |= migrate_mcp_servers(&config_dir, extensions);
    migrated |= migrate_a2a_agents(&config_dir, extensions);
    migrated
}

fn migrate_mcp_servers(config_dir: &Path, extensions: &mut ExtensionsModel) -> bool {
    let path = config_dir.join("mcp_servers.json");
    let data = match std::fs::read_to_string(&path) {
        Ok(d) => d,
        Err(_) => return false,
    };

    let servers: Vec<McpServerConfig> = match serde_json::from_str(&data) {
        Ok(s) => s,
        Err(_) => return false,
    };

    let mut migrated = false;
    for server in servers {
        // Skip module-registered MCP entries — they are ephemeral
        if server.is_module {
            continue;
        }

        let id = format!("mcp-{}", server.name);
        if extensions.is_installed(&id) {
            continue;
        }

        extensions.add(InstalledExtension {
            id,
            display_name: server.name.clone(),
            description: format!("MCP server at {}", server.url),
            kind: ExtensionKind::McpServer(server),
            source: ExtensionSource::Custom,
            enabled: true,
        });
        migrated = true;
    }
    migrated
}

fn migrate_a2a_agents(config_dir: &Path, extensions: &mut ExtensionsModel) -> bool {
    let path = config_dir.join("a2a_agents.json");
    let data = match std::fs::read_to_string(&path) {
        Ok(d) => d,
        Err(_) => return false,
    };

    let agents: Vec<A2aAgentConfig> = match serde_json::from_str(&data) {
        Ok(a) => a,
        Err(_) => return false,
    };

    let mut migrated = false;
    for agent in agents {
        let id = format!("a2a-{}", agent.name);
        if extensions.is_installed(&id) {
            continue;
        }

        extensions.add(InstalledExtension {
            id,
            display_name: agent.name.clone(),
            description: format!("A2A agent at {}", agent.url),
            kind: ExtensionKind::A2aAgent(agent),
            source: ExtensionSource::Custom,
            enabled: true,
        });
        migrated = true;
    }
    migrated
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrate_mcp_servers_from_json() {
        let mut model = ExtensionsModel::default();
        let tmp = tempfile::tempdir().unwrap();
        let servers = vec![
            McpServerConfig {
                name: "github".into(),
                url: "http://localhost:3000/mcp".into(),
                api_key: None,
                enabled: true,
                is_module: false,
            },
            McpServerConfig {
                name: "module-auto".into(),
                url: "http://localhost:8420/mcp".into(),
                api_key: None,
                enabled: true,
                is_module: true, // should be skipped
            },
        ];
        std::fs::write(
            tmp.path().join("mcp_servers.json"),
            serde_json::to_string(&servers).unwrap(),
        )
        .unwrap();

        let migrated = migrate_mcp_servers(&tmp.path().to_path_buf(), &mut model);
        assert!(migrated);
        assert_eq!(model.extensions.len(), 1);
        assert_eq!(model.extensions[0].id, "mcp-github");
        assert!(matches!(
            model.extensions[0].kind,
            ExtensionKind::McpServer(_)
        ));
    }

    #[test]
    fn migrate_a2a_agents_from_json() {
        let mut model = ExtensionsModel::default();
        let tmp = tempfile::tempdir().unwrap();
        let agents = vec![A2aAgentConfig {
            name: "voucher".into(),
            url: "https://example.com/a2a".into(),
            api_key: None,
            enabled: true,
            skills: vec!["apply-voucher".into()],
        }];
        std::fs::write(
            tmp.path().join("a2a_agents.json"),
            serde_json::to_string(&agents).unwrap(),
        )
        .unwrap();

        let migrated = migrate_a2a_agents(&tmp.path().to_path_buf(), &mut model);
        assert!(migrated);
        assert_eq!(model.extensions.len(), 1);
        assert_eq!(model.extensions[0].id, "a2a-voucher");
    }

    #[test]
    fn migrate_skips_duplicates() {
        let mut model = ExtensionsModel::default();
        model.add(InstalledExtension {
            id: "mcp-github".into(),
            display_name: "GitHub".into(),
            description: "".into(),
            kind: ExtensionKind::McpServer(McpServerConfig {
                name: "github".into(),
                url: "http://localhost:3000/mcp".into(),
                api_key: None,
                enabled: true,
                is_module: false,
            }),
            source: ExtensionSource::Custom,
            enabled: true,
        });

        let tmp = tempfile::tempdir().unwrap();
        let servers = vec![McpServerConfig {
            name: "github".into(),
            url: "http://localhost:3000/mcp".into(),
            api_key: None,
            enabled: true,
            is_module: false,
        }];
        std::fs::write(
            tmp.path().join("mcp_servers.json"),
            serde_json::to_string(&servers).unwrap(),
        )
        .unwrap();

        let migrated = migrate_mcp_servers(&tmp.path().to_path_buf(), &mut model);
        assert!(!migrated);
        assert_eq!(model.extensions.len(), 1);
    }

    #[test]
    fn migrate_missing_files_returns_false() {
        let mut model = ExtensionsModel::default();
        let tmp = tempfile::tempdir().unwrap();
        assert!(!migrate_mcp_servers(&tmp.path().to_path_buf(), &mut model));
        assert!(!migrate_a2a_agents(&tmp.path().to_path_buf(), &mut model));
    }
}
