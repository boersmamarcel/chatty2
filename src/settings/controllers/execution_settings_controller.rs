use crate::EXECUTION_SETTINGS_REPOSITORY;
use crate::settings::models::execution_settings::{ApprovalMode, ExecutionSettingsModel};
use crate::settings::models::{GlobalMcpNotifier, McpNotifierEvent};
use gpui::{App, AsyncApp};
use tracing::{error, info, warn};

/// Emit `ServersUpdated` so the active conversation's agent is rebuilt
/// with the current execution tool settings (bash, filesystem, MCP management).
fn notify_tool_set_changed(cx: &mut App) {
    if let Some(weak_notifier) = cx
        .try_global::<GlobalMcpNotifier>()
        .and_then(|g| g.entity.clone())
        && let Some(notifier) = weak_notifier.upgrade()
    {
        info!("Notifying tool set changed — triggering agent rebuild");
        notifier.update(cx, |_notifier, cx| {
            cx.emit(McpNotifierEvent::ServersUpdated);
        });
    } else {
        warn!("notify_tool_set_changed: GlobalMcpNotifier not found — agent will not be rebuilt");
    }
}

/// Toggle code execution enabled/disabled and persist to disk
pub fn toggle_execution(cx: &mut App) {
    // 1. Apply update immediately (optimistic update)
    let old_enabled = cx.global::<ExecutionSettingsModel>().enabled;
    let new_enabled = !old_enabled;
    info!(
        old = old_enabled,
        new = new_enabled,
        "Toggling code execution"
    );
    cx.global_mut::<ExecutionSettingsModel>().enabled = new_enabled;

    // 2. Get updated state for async save
    let settings = cx.global::<ExecutionSettingsModel>().clone();

    // 3. Refresh UI immediately (optimistic update)
    cx.refresh_windows();

    // 4. Notify so the active conversation's agent is rebuilt with the new tool set
    notify_tool_set_changed(cx);

    // 5. Save async with error handling
    cx.spawn(|_cx: &mut AsyncApp| async move {
        let repo = EXECUTION_SETTINGS_REPOSITORY.clone();
        if let Err(e) = repo.save(settings).await {
            error!(error = ?e, "Failed to save execution settings");
        }
    })
    .detach();
}

/// Update workspace directory and persist to disk
pub fn set_workspace_dir(dir: Option<String>, cx: &mut App) {
    // 1. Apply update immediately
    cx.global_mut::<ExecutionSettingsModel>().workspace_dir = dir;

    // 2. Get updated state for async save
    let settings = cx.global::<ExecutionSettingsModel>().clone();

    // 3. Refresh UI immediately
    cx.refresh_windows();

    // 4. Notify so the active conversation's agent is rebuilt (workspace dir affects fs tools)
    notify_tool_set_changed(cx);

    // 5. Save async with error handling
    cx.spawn(|_cx: &mut AsyncApp| async move {
        let repo = EXECUTION_SETTINGS_REPOSITORY.clone();
        if let Err(e) = repo.save(settings).await {
            error!(error = ?e, "Failed to save execution settings");
        }
    })
    .detach();
}

/// Update approval mode and persist to disk
pub fn set_approval_mode(mode: ApprovalMode, cx: &mut App) {
    // 1. Apply update immediately
    cx.global_mut::<ExecutionSettingsModel>().approval_mode = mode;

    // 2. Get updated state for async save
    let settings = cx.global::<ExecutionSettingsModel>().clone();

    // 3. Refresh UI immediately
    cx.refresh_windows();

    // 4. Save async with error handling
    cx.spawn(|_cx: &mut AsyncApp| async move {
        let repo = EXECUTION_SETTINGS_REPOSITORY.clone();
        if let Err(e) = repo.save(settings).await {
            error!(error = ?e, "Failed to save execution settings");
        }
    })
    .detach();
}

/// Toggle filesystem read tools enabled/disabled and persist to disk
pub fn toggle_filesystem_read(cx: &mut App) {
    // 1. Apply update immediately (optimistic update)
    let new_enabled = !cx
        .global::<ExecutionSettingsModel>()
        .filesystem_read_enabled;
    cx.global_mut::<ExecutionSettingsModel>()
        .filesystem_read_enabled = new_enabled;

    // 2. Get updated state for async save
    let settings = cx.global::<ExecutionSettingsModel>().clone();

    // 3. Refresh UI immediately (optimistic update)
    cx.refresh_windows();

    // 4. Notify so the active conversation's agent is rebuilt with the new tool set
    notify_tool_set_changed(cx);

    // 5. Save async with error handling
    cx.spawn(|_cx: &mut AsyncApp| async move {
        let repo = EXECUTION_SETTINGS_REPOSITORY.clone();
        if let Err(e) = repo.save(settings).await {
            error!(error = ?e, "Failed to save execution settings");
        }
    })
    .detach();
}

/// Toggle network isolation enabled/disabled and persist to disk
pub fn toggle_network_isolation(cx: &mut App) {
    // 1. Apply update immediately (optimistic update)
    let new_isolation = !cx.global::<ExecutionSettingsModel>().network_isolation;
    cx.global_mut::<ExecutionSettingsModel>().network_isolation = new_isolation;

    // 2. Get updated state for async save
    let settings = cx.global::<ExecutionSettingsModel>().clone();

    // 3. Refresh UI immediately (optimistic update)
    cx.refresh_windows();

    // 4. Save async with error handling
    cx.spawn(|_cx: &mut AsyncApp| async move {
        let repo = EXECUTION_SETTINGS_REPOSITORY.clone();
        if let Err(e) = repo.save(settings).await {
            error!(error = ?e, "Failed to save execution settings");
        }
    })
    .detach();
}

/// Toggle the add_mcp_service tool enabled/disabled and persist to disk.
/// When disabled, the LLM cannot register new MCP servers.
pub fn toggle_mcp_service_tool(cx: &mut App) {
    // 1. Apply update immediately (optimistic update)
    let new_enabled = !cx
        .global::<ExecutionSettingsModel>()
        .mcp_service_tool_enabled;
    cx.global_mut::<ExecutionSettingsModel>()
        .mcp_service_tool_enabled = new_enabled;

    // 2. Get updated state for async save
    let settings = cx.global::<ExecutionSettingsModel>().clone();

    // 3. Refresh UI immediately (optimistic update)
    cx.refresh_windows();

    // 4. Notify so the active conversation's agent is rebuilt with the new tool set
    notify_tool_set_changed(cx);

    // 5. Save async with error handling
    cx.spawn(|_cx: &mut AsyncApp| async move {
        let repo = EXECUTION_SETTINGS_REPOSITORY.clone();
        if let Err(e) = repo.save(settings).await {
            error!(error = ?e, "Failed to save execution settings");
        }
    })
    .detach();
}

/// Toggle the built-in fetch tool enabled/disabled and persist to disk.
pub fn toggle_fetch(cx: &mut App) {
    // 1. Apply update immediately (optimistic update)
    let new_enabled = !cx.global::<ExecutionSettingsModel>().fetch_enabled;
    cx.global_mut::<ExecutionSettingsModel>().fetch_enabled = new_enabled;

    // 2. Get updated state for async save
    let settings = cx.global::<ExecutionSettingsModel>().clone();

    // 3. Refresh UI immediately (optimistic update)
    cx.refresh_windows();

    // 4. Notify so the active conversation's agent is rebuilt with the new tool set
    notify_tool_set_changed(cx);

    // 5. Save async with error handling
    cx.spawn(|_cx: &mut AsyncApp| async move {
        let repo = EXECUTION_SETTINGS_REPOSITORY.clone();
        if let Err(e) = repo.save(settings).await {
            error!(error = ?e, "Failed to save execution settings");
        }
    })
    .detach();
}

/// Toggle filesystem write tools enabled/disabled and persist to disk
pub fn toggle_filesystem_write(cx: &mut App) {
    // 1. Apply update immediately (optimistic update)
    let new_enabled = !cx
        .global::<ExecutionSettingsModel>()
        .filesystem_write_enabled;
    cx.global_mut::<ExecutionSettingsModel>()
        .filesystem_write_enabled = new_enabled;

    // 2. Get updated state for async save
    let settings = cx.global::<ExecutionSettingsModel>().clone();

    // 3. Refresh UI immediately (optimistic update)
    cx.refresh_windows();

    // 4. Notify so the active conversation's agent is rebuilt with the new tool set
    notify_tool_set_changed(cx);

    // 5. Save async with error handling
    cx.spawn(|_cx: &mut AsyncApp| async move {
        let repo = EXECUTION_SETTINGS_REPOSITORY.clone();
        if let Err(e) = repo.save(settings).await {
            error!(error = ?e, "Failed to save execution settings");
        }
    })
    .detach();
}
