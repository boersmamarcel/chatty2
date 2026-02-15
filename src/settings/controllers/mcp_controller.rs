use crate::chatty::services::mcp_service::McpService;
use crate::settings::models::mcp_store::{McpServerConfig, McpServersModel};
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

    // 3. Update MCP service (start/stop server)
    if let Some(config) = updated_servers
        .iter()
        .find(|s| s.name == server_name)
        .cloned()
    {
        let service = cx.global::<McpService>().clone();
        let name = config.name.clone();
        let is_enabled = config.enabled;

        cx.spawn(move |_cx: &mut AsyncApp| async move {
            if is_enabled {
                if let Err(e) = service.start_server(config).await {
                    error!(server = %name, error = ?e, "Failed to start MCP server");
                }
            } else {
                if let Err(e) = service.stop_server(&name).await {
                    error!(server = %name, error = ?e, "Failed to stop MCP server");
                }
            }
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
