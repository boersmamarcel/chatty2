use crate::settings::models::mcp_store::{McpServerConfig, McpServersModel};
use gpui::{App, AsyncApp};
use tracing::error;

/// Create a new MCP server configuration
pub fn create_mcp_server(config: McpServerConfig, cx: &mut App) {
    // 1. Apply update immediately (optimistic update)
    let model = cx.global_mut::<McpServersModel>();
    model.add_server(config);

    // 2. Get updated state for async save
    let servers_to_save = cx.global::<McpServersModel>().servers().to_vec();

    // 3. Refresh UI immediately (optimistic update)
    cx.refresh_windows();

    // 4. Save async with error handling
    save_mcp_servers_async(servers_to_save, cx);
}

/// Update an existing MCP server configuration
pub fn update_mcp_server(name: String, config: McpServerConfig, cx: &mut App) {
    // 1. Apply update immediately (optimistic update)
    let model = cx.global_mut::<McpServersModel>();

    if !model.update_server(&name, config) {
        error!("Failed to update MCP server: server not found");
        return;
    }

    // 2. Get updated state for async save
    let servers_to_save = cx.global::<McpServersModel>().servers().to_vec();

    // 3. Refresh UI immediately (optimistic update)
    cx.refresh_windows();

    // 4. Save async with error handling
    save_mcp_servers_async(servers_to_save, cx);
}

/// Delete an MCP server by name
pub fn delete_mcp_server(name: String, cx: &mut App) {
    // 1. Apply update immediately (optimistic update)
    let model = cx.global_mut::<McpServersModel>();

    if !model.remove_server(&name) {
        error!("Failed to delete MCP server: server not found");
        return;
    }

    // 2. Get updated state for async save
    let servers_to_save = cx.global::<McpServersModel>().servers().to_vec();

    // 3. Refresh UI immediately (optimistic update)
    cx.refresh_windows();

    // 4. Save async with error handling
    save_mcp_servers_async(servers_to_save, cx);
}

/// Toggle enabled status of an MCP server
pub fn toggle_mcp_server(name: String, enabled: bool, cx: &mut App) {
    // 1. Apply update immediately (optimistic update)
    let model = cx.global_mut::<McpServersModel>();

    if let Some(server) = model.servers_mut().iter_mut().find(|s| s.name == name) {
        server.enabled = enabled;
    } else {
        error!("Failed to toggle MCP server: server not found");
        return;
    }

    // 2. Get updated state for async save
    let servers_to_save = cx.global::<McpServersModel>().servers().to_vec();

    // 3. Refresh UI immediately (optimistic update)
    cx.refresh_windows();

    // 4. Save async with error handling
    save_mcp_servers_async(servers_to_save, cx);
}

/// Save MCP servers asynchronously to disk
fn save_mcp_servers_async(servers: Vec<McpServerConfig>, cx: &mut App) {
    use crate::MCP_REPOSITORY;

    cx.spawn(|_cx: &mut AsyncApp| async move {
        let repo = MCP_REPOSITORY.clone();
        if let Err(e) = repo.save_all(servers).await {
            error!(error = ?e, "Failed to save MCP servers, changes will be lost on restart");
        }
    })
    .detach();
}
