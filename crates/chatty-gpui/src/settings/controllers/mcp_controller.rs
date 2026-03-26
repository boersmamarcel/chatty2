use crate::chatty::services::mcp_service::McpService;
use crate::settings::models::mcp_store::{McpAuthStatus, McpServerConfig, McpServersModel};
use crate::settings::models::{AgentConfigEvent, GlobalAgentConfigNotifier};
use gpui::{App, AsyncApp};
use tracing::{error, info};

/// Create a new MCP server and persist to disk. The server starts disabled.
pub fn create_server(name: String, url: String, api_key: Option<String>, cx: &mut App) {
    let name = name.trim().to_string();
    let url = url.trim().to_string();
    let api_key = api_key.filter(|k| !k.trim().is_empty());

    // 1. Add to global state (optimistic, starts disabled)
    let updated_servers = {
        let model = cx.global_mut::<McpServersModel>();
        model.servers_mut().push(McpServerConfig {
            name: name.clone(),
            url: url.clone(),
            api_key,
            enabled: false,
        });
        model.servers().to_vec()
    };

    // 2. Refresh UI immediately
    cx.refresh_windows();

    // 3. Save async to disk
    save_servers_async(updated_servers, cx);

    info!(server = %name, url = %url, "Created new MCP server");
}

/// Update the API key for an existing MCP server and persist to disk.
/// If the server is currently enabled, it will be reconnected with the new key.
pub fn update_server_api_key(server_name: String, api_key: Option<String>, cx: &mut App) {
    let api_key = api_key.filter(|k| !k.trim().is_empty());

    // 1. Update in global state (optimistic)
    let (updated_servers, was_enabled) = {
        let model = cx.global_mut::<McpServersModel>();
        let was_enabled = if let Some(server) = model
            .servers_mut()
            .iter_mut()
            .find(|s| s.name == server_name)
        {
            let enabled = server.enabled;
            server.api_key = api_key;
            enabled
        } else {
            error!(server = %server_name, "Server not found for API key update");
            return;
        };
        (model.servers().to_vec(), was_enabled)
    };

    // 2. Refresh UI immediately
    cx.refresh_windows();

    // 3. Reconnect if the server was enabled so the new key takes effect
    if was_enabled {
        if let Some(config) = updated_servers
            .iter()
            .find(|s| s.name == server_name)
            .cloned()
        {
            let service = cx.global::<McpService>().clone();
            let name = config.name.clone();

            cx.spawn(async move |cx| {
                if let Err(e) = service.disconnect_server(&name).await {
                    error!(server = %name, error = ?e, "Failed to disconnect for API key update");
                }
                if let Err(e) = service.connect_server(config).await {
                    error!(server = %name, error = ?e, "Failed to reconnect with new API key");
                }

                cx.update(|cx| {
                    emit_rebuild_required(cx);
                })
                .ok();
            })
            .detach();
        }
    }

    // 4. Save async to disk
    save_servers_async(updated_servers, cx);

    info!(server = %server_name, "Updated MCP server API key");
}

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

    // 3. Connect to or disconnect from the server, then notify subscribers so
    //    the active conversation's agent is rebuilt with the updated tool set.
    if let Some(config) = updated_servers
        .iter()
        .find(|s| s.name == server_name)
        .cloned()
    {
        let service = cx.global::<McpService>().clone();
        let name = config.name.clone();
        let is_enabled = config.enabled;

        if is_enabled {
            // Set Connecting status
            cx.global_mut::<McpServersModel>()
                .set_auth_status(name.clone(), McpAuthStatus::Connecting);
            cx.refresh_windows();
        }

        cx.spawn(async move |cx| {
            if is_enabled {
                match service.connect_server(config).await {
                    Ok(()) => {
                        cx.update(|cx| {
                            cx.global_mut::<McpServersModel>()
                                .set_auth_status(name.clone(), McpAuthStatus::Authenticated);
                            cx.refresh_windows();
                        })
                        .ok();
                    }
                    Err(e) => {
                        let err_msg = format!("{e:#}");
                        error!(server = %name, error = ?e, "Failed to connect to MCP server");
                        cx.update(|cx| {
                            let status = if err_msg.contains("Auth required")
                                || err_msg.contains("AuthRequired")
                            {
                                McpAuthStatus::NeedsAuth
                            } else {
                                McpAuthStatus::Failed(err_msg)
                            };
                            cx.global_mut::<McpServersModel>()
                                .set_auth_status(name.clone(), status);
                            cx.refresh_windows();
                        })
                        .ok();
                        return;
                    }
                }
            } else {
                if let Err(e) = service.disconnect_server(&name).await {
                    error!(server = %name, error = ?e, "Failed to disconnect from MCP server");
                }
                cx.update(|cx| {
                    cx.global_mut::<McpServersModel>()
                        .set_auth_status(name.clone(), McpAuthStatus::NotRequired);
                    cx.refresh_windows();
                })
                .ok();
            }

            // Emit RebuildRequired after connect/disconnect completes so subscribers
            // (e.g. ChattyApp) rebuild the active conversation's agent with
            // the now-accurate tool set.
            cx.update(|cx| {
                emit_rebuild_required(cx);
            })
            .map_err(|e| error!(error = ?e, "Failed to emit RebuildRequired after MCP toggle"))
            .ok();
        })
        .detach();
    }

    // 4. Save async to disk
    save_servers_async(updated_servers, cx);
}

/// Delete an MCP server by name, disconnect it if connected, and persist to disk.
pub fn delete_server(server_name: String, cx: &mut App) {
    // 1. Remove from global state (optimistic update)
    let (was_enabled, updated_servers) = {
        let model = cx.global_mut::<McpServersModel>();
        let was_enabled = model
            .servers()
            .iter()
            .find(|s| s.name == server_name)
            .map(|s| s.enabled)
            .unwrap_or(false);

        model.servers_mut().retain(|s| s.name != server_name);
        model.remove_auth_status(&server_name);
        (was_enabled, model.servers().to_vec())
    };

    // 2. Refresh UI immediately
    cx.refresh_windows();

    // 3. Disconnect from the server if it was enabled, clean up credentials,
    //    then emit RebuildRequired
    let name_for_cleanup = server_name.clone();
    if was_enabled {
        let service = cx.global::<McpService>().clone();
        let name = server_name.clone();

        cx.spawn(async move |cx| {
            if let Err(e) = service.disconnect_server(&name).await {
                error!(server = %name, error = ?e, "Failed to disconnect deleted MCP server");
            }

            // Clean up OAuth credentials
            McpService::delete_server_credentials(&name_for_cleanup).await;

            cx.update(|cx| {
                emit_rebuild_required(cx);
            })
            .map_err(|e| error!(error = ?e, "Failed to emit RebuildRequired after MCP delete"))
            .ok();
        })
        .detach();
    } else {
        // Still clean up credentials even if server wasn't enabled
        cx.spawn(|_cx: &mut AsyncApp| async move {
            McpService::delete_server_credentials(&name_for_cleanup).await;
        })
        .detach();
        emit_rebuild_required(cx);
    }

    // 4. Save async to disk
    save_servers_async(updated_servers, cx);

    info!(server = %server_name, "Deleted MCP server from settings");
}

/// Emit the ServersUpdated event via the global MCP notifier.
fn emit_rebuild_required(cx: &mut App) {
    if let Some(weak_notifier) = cx
        .try_global::<GlobalAgentConfigNotifier>()
        .and_then(|g| g.entity.clone())
        && let Some(notifier) = weak_notifier.upgrade()
    {
        notifier.update(cx, |_notifier, cx| {
            cx.emit(AgentConfigEvent::RebuildRequired);
        });
    }
}

/// Save servers asynchronously to disk
fn save_servers_async(servers: Vec<McpServerConfig>, cx: &mut App) {
    cx.spawn(|_cx: &mut AsyncApp| async move {
        let repo = chatty_core::mcp_repository();
        if let Err(e) = repo.save_all(servers).await {
            error!(error = ?e, "Failed to save MCP servers, changes will be lost on restart");
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: Create a test server config
    fn test_server_config(name: &str, enabled: bool) -> McpServerConfig {
        McpServerConfig {
            name: name.to_string(),
            url: "http://localhost:3000/mcp".to_string(),
            api_key: None,
            enabled,
        }
    }

    #[test]
    fn test_toggle_nonexistent_server_logic() {
        let servers = [
            test_server_config("server-1", true),
            test_server_config("server-2", false),
        ];

        let result = servers.iter().find(|s| s.name == "nonexistent");
        assert!(result.is_none());
    }

    #[test]
    fn test_toggle_server_enable_logic() {
        let mut servers = [test_server_config("test-server", false)];
        assert!(!servers[0].enabled);

        if let Some(server) = servers.iter_mut().find(|s| s.name == "test-server") {
            server.enabled = !server.enabled;
        }
        assert!(servers[0].enabled);
    }

    #[test]
    fn test_toggle_server_disable_logic() {
        let mut servers = [test_server_config("test-server", true)];
        assert!(servers[0].enabled);

        if let Some(server) = servers.iter_mut().find(|s| s.name == "test-server") {
            server.enabled = !server.enabled;
        }
        assert!(!servers[0].enabled);
    }

    #[test]
    fn test_toggle_preserves_other_servers() {
        let mut servers = [
            test_server_config("server-1", true),
            test_server_config("server-2", false),
            test_server_config("server-3", true),
        ];

        if let Some(server) = servers.iter_mut().find(|s| s.name == "server-2") {
            server.enabled = !server.enabled;
        }

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
        let mut servers = [test_server_config("test-server", false)];

        for _ in 0..3 {
            if let Some(server) = servers.iter_mut().find(|s| s.name == "test-server") {
                server.enabled = !server.enabled;
            }
        }
        assert!(servers[0].enabled);
    }

    #[test]
    fn test_server_config_preserves_fields_on_toggle() {
        let mut servers = [McpServerConfig {
            name: "test".to_string(),
            url: "http://localhost:9000/mcp".to_string(),
            api_key: Some("bearer-token".to_string()),
            enabled: false,
        }];

        if let Some(server) = servers.iter_mut().find(|s| s.name == "test") {
            server.enabled = !server.enabled;
        }

        assert_eq!(servers[0].url, "http://localhost:9000/mcp");
        assert_eq!(servers[0].api_key.as_deref(), Some("bearer-token"));
        assert!(servers[0].enabled);
    }
}
