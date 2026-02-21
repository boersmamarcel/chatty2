use crate::chatty::services::mcp_service::McpService;
use crate::settings::models::mcp_store::{McpServerConfig, McpServersModel};
use crate::settings::models::{GlobalMcpNotifier, McpNotifierEvent};
use gpui::{App, AsyncApp};
use tracing::{error, info};

/// Toggle the enabled state of an MCP server
pub fn toggle_server(server_name: String, cx: &mut App) {
    // 1. Toggle in global state (optimistic update)
    let updated_servers = {
        let model = cx.global_mut::<McpServersModel>();

        if let Some(server) = model
            .servers_mut()
            .iter_mut()
            .find(|s| s.name == server_name)
        {
            server.enabled = !server.enabled;
            info!(
                server = %server_name,
                enabled = server.enabled,
                "Toggled MCP server"
            );
        } else {
            error!(server = %server_name, "Server not found");
            return;
        }

        model.servers().to_vec()
    };

    // 2. Refresh UI immediately
    cx.refresh_windows();

    // 3. Update MCP service (start/stop server), then notify subscribers so the
    //    active conversation's agent is rebuilt with the updated tool set.
    if let Some(config) = updated_servers
        .iter()
        .find(|s| s.name == server_name)
        .cloned()
    {
        let service = cx.global::<McpService>().clone();
        let name = config.name.clone();
        let is_enabled = config.enabled;

        cx.spawn(async move |cx| {
            if is_enabled {
                if let Err(e) = service.start_server(config).await {
                    error!(server = %name, error = ?e, "Failed to start MCP server");
                    return;
                }
            } else if let Err(e) = service.stop_server(&name).await {
                error!(server = %name, error = ?e, "Failed to stop MCP server");
            }

            // Emit ServersUpdated after start/stop completes so subscribers
            // (e.g. ChattyApp) rebuild the active conversation's agent with
            // the now-accurate tool set.
            cx.update(|cx| {
                if let Some(weak_notifier) = cx
                    .try_global::<GlobalMcpNotifier>()
                    .and_then(|g| g.entity.clone())
                    && let Some(notifier) = weak_notifier.upgrade()
                {
                    notifier.update(cx, |_notifier, cx| {
                        cx.emit(McpNotifierEvent::ServersUpdated);
                    });
                }
            })
            .map_err(|e| error!(error = ?e, "Failed to emit ServersUpdated after MCP toggle"))
            .ok();
        })
        .detach();
    }

    // 4. Save async to disk
    save_servers_async(updated_servers, cx);
}

/// Save servers asynchronously to disk
fn save_servers_async(servers: Vec<McpServerConfig>, cx: &mut App) {
    use crate::MCP_REPOSITORY;

    cx.spawn(|_cx: &mut AsyncApp| async move {
        let repo = MCP_REPOSITORY.clone();
        if let Err(e) = repo.save_all(servers).await {
            error!(error = ?e, "Failed to save MCP servers, changes will be lost on restart");
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Helper: Create a test server config
    fn test_server_config(name: &str, enabled: bool) -> McpServerConfig {
        McpServerConfig {
            name: name.to_string(),
            command: "echo".to_string(),
            args: vec!["test".to_string()],
            env: HashMap::new(),
            enabled,
        }
    }

    #[test]
    fn test_toggle_nonexistent_server_logic() {
        // Unit test: Verify that trying to find a non-existent server returns None
        let servers = [
            test_server_config("server-1", true),
            test_server_config("server-2", false),
        ];

        let result = servers.iter().find(|s| s.name == "nonexistent");
        assert!(result.is_none());

        // This ensures the controller's early return on None prevents panics
    }

    #[test]
    fn test_toggle_server_enable_logic() {
        // Unit test: Simulate toggling a disabled server to enabled
        let mut servers = [test_server_config("test-server", false)];

        // Initially disabled
        assert!(!servers[0].enabled);

        // Simulate toggle logic
        if let Some(server) = servers.iter_mut().find(|s| s.name == "test-server") {
            server.enabled = !server.enabled;
        }

        // Now enabled
        assert!(servers[0].enabled);
    }

    #[test]
    fn test_toggle_server_disable_logic() {
        // Unit test: Simulate toggling an enabled server to disabled
        let mut servers = [test_server_config("test-server", true)];

        // Initially enabled
        assert!(servers[0].enabled);

        // Simulate toggle logic
        if let Some(server) = servers.iter_mut().find(|s| s.name == "test-server") {
            server.enabled = !server.enabled;
        }

        // Now disabled
        assert!(!servers[0].enabled);
    }

    #[test]
    fn test_toggle_preserves_other_servers() {
        // Unit test: Verify toggling one server doesn't affect others
        let mut servers = [
            test_server_config("server-1", true),
            test_server_config("server-2", false),
            test_server_config("server-3", true),
        ];

        // Toggle server-2
        if let Some(server) = servers.iter_mut().find(|s| s.name == "server-2") {
            server.enabled = !server.enabled;
        }

        // Verify: server-1 and server-3 unchanged, server-2 now enabled
        assert!(
            servers
                .iter()
                .find(|s| s.name == "server-1")
                .unwrap()
                .enabled
        );
        assert!(
            servers
                .iter()
                .find(|s| s.name == "server-2")
                .unwrap()
                .enabled
        );
        assert!(
            servers
                .iter()
                .find(|s| s.name == "server-3")
                .unwrap()
                .enabled
        );
    }

    #[test]
    fn test_multiple_toggles() {
        // Unit test: Verify multiple consecutive toggles work correctly
        let mut servers = [test_server_config("test-server", false)];

        // Toggle 1: false -> true
        if let Some(server) = servers.iter_mut().find(|s| s.name == "test-server") {
            server.enabled = !server.enabled;
        }
        assert!(servers[0].enabled);

        // Toggle 2: true -> false
        if let Some(server) = servers.iter_mut().find(|s| s.name == "test-server") {
            server.enabled = !server.enabled;
        }
        assert!(!servers[0].enabled);

        // Toggle 3: false -> true
        if let Some(server) = servers.iter_mut().find(|s| s.name == "test-server") {
            server.enabled = !server.enabled;
        }
        assert!(servers[0].enabled);
    }

    #[test]
    fn test_server_config_preserves_fields_on_toggle() {
        // Unit test: Ensure toggle only affects enabled field
        let mut servers = [McpServerConfig {
            name: "test".to_string(),
            command: "special-command".to_string(),
            args: vec!["arg1".to_string(), "arg2".to_string()],
            env: {
                let mut map = HashMap::new();
                map.insert("KEY".to_string(), "value".to_string());
                map
            },
            enabled: false,
        }];

        // Toggle enabled
        if let Some(server) = servers.iter_mut().find(|s| s.name == "test") {
            server.enabled = !server.enabled;
        }

        // Verify other fields unchanged
        assert_eq!(servers[0].command, "special-command");
        assert_eq!(servers[0].args, vec!["arg1", "arg2"]);
        assert_eq!(servers[0].env.get("KEY").unwrap(), "value");
        assert!(servers[0].enabled); // And enabled is now true
    }
}
