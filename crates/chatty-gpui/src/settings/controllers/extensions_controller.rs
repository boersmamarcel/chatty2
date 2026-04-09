use crate::chatty::services::mcp_service::McpService;
use crate::settings::controllers::module_settings_controller;
use crate::settings::models::marketplace_state::MarketplaceState;
use crate::settings::models::{AgentConfigEvent, GlobalAgentConfigNotifier};
use chatty_core::hive::HiveRegistryClient;
use chatty_core::install;
use chatty_core::services::A2aClient;
use chatty_core::settings::models::a2a_store::A2aAgentStatus;
use chatty_core::settings::models::extensions_store::{
    ExtensionKind, ExtensionSource, ExtensionsModel, InstalledExtension,
};
use chatty_core::settings::models::hive_settings::HiveSettingsModel;
use chatty_core::settings::models::mcp_store::{McpAuthStatus, McpServerConfig, McpServersModel};
use gpui::{App, AsyncApp};
use tracing::{error, info, warn};

// ── Default Hive MCP ──────────────────────────────────────────────────────

/// Ensure the Hive registry MCP server is present in the Extensions store
/// and in McpServersModel. Thin GPUI wrapper around [`chatty_core::install::ensure_default_hive_mcp`].
///
/// Returns `true` if a new entry was added (caller should persist).
pub fn ensure_default_hive_mcp(cx: &mut App) -> bool {
    let registry_url = cx.global::<HiveSettingsModel>().registry_url.clone();
    let mut mcp_servers = cx.global::<McpServersModel>().servers().to_vec();

    let extensions = cx.global_mut::<ExtensionsModel>();
    let added = install::ensure_default_hive_mcp(&registry_url, extensions, &mut mcp_servers);

    if added {
        let mcp_model = cx.global_mut::<McpServersModel>();
        mcp_model.replace_all(mcp_servers);
        info!("Added default Hive MCP server extension (disabled)");
    }

    added
}

// ── Authentication ─────────────────────────────────────────────────────────

/// Log in to the Hive registry and persist credentials.
pub fn login(email: String, password: String, cx: &mut App) {
    let registry_url = cx.global::<HiveSettingsModel>().registry_url.clone();
    let client = HiveRegistryClient::new(&registry_url);

    cx.spawn(
        async move |cx| match client.login(&email, &password).await {
            Ok(auth) => {
                let username = auth.username().unwrap_or_default();
                let token = auth.token.clone();
                cx.update(|cx| {
                    let settings = cx.global_mut::<HiveSettingsModel>();
                    settings.token = Some(auth.token);
                    settings.username = Some(username.clone());
                    settings.email = Some(email);
                    save_hive_settings_async(settings.clone(), cx);
                    sync_hive_token_to_mcp(Some(token), cx);
                    cx.refresh_windows();
                    info!(username = %username, "Logged in to Hive registry");
                })
                .ok();
            }
            Err(e) => {
                error!(error = ?e, "Hive login failed");
                cx.update(|cx| {
                    let state = cx.global_mut::<MarketplaceState>();
                    state.set_error(format!("Login failed: {e}"));
                    cx.refresh_windows();
                })
                .ok();
            }
        },
    )
    .detach();
}

/// Register a new account on the Hive registry.
pub fn register(username: String, email: String, password: String, cx: &mut App) {
    let registry_url = cx.global::<HiveSettingsModel>().registry_url.clone();
    let client = HiveRegistryClient::new(&registry_url);

    cx.spawn(
        async move |cx| match client.register(&username, &email, &password).await {
            Ok(auth) => {
                let token = auth.token.clone();
                cx.update(|cx| {
                    let settings = cx.global_mut::<HiveSettingsModel>();
                    settings.token = Some(auth.token);
                    settings.username = Some(username.clone());
                    settings.email = Some(email);
                    save_hive_settings_async(settings.clone(), cx);
                    sync_hive_token_to_mcp(Some(token), cx);
                    cx.refresh_windows();
                    info!(username = %username, "Registered with Hive registry");
                })
                .ok();
            }
            Err(e) => {
                error!(error = ?e, "Hive registration failed");
                cx.update(|cx| {
                    let state = cx.global_mut::<MarketplaceState>();
                    state.set_error(format!("Registration failed: {e}"));
                    cx.refresh_windows();
                })
                .ok();
            }
        },
    )
    .detach();
}

/// Log out: clear persisted credentials.
pub fn logout(cx: &mut App) {
    let settings = cx.global_mut::<HiveSettingsModel>();
    settings.token = None;
    settings.username = None;
    settings.email = None;
    save_hive_settings_async(settings.clone(), cx);
    sync_hive_token_to_mcp(None, cx);
    cx.refresh_windows();
    info!("Logged out from Hive registry");
}

// ── Marketplace browsing ───────────────────────────────────────────────────

/// Search the marketplace for extensions matching `query`.
pub fn search_marketplace(query: String, cx: &mut App) {
    let registry_url = cx.global::<HiveSettingsModel>().registry_url.clone();
    let client = HiveRegistryClient::new(&registry_url);

    {
        let state = cx.global_mut::<MarketplaceState>();
        state.search_query = query.clone();
        state.set_loading();
    }
    cx.refresh_windows();

    cx.spawn(async move |cx| {
        let result = if query.is_empty() {
            client.list_modules(&Default::default()).await
        } else {
            client.search(&query).await
        };

        cx.update(|cx| {
            let state = cx.global_mut::<MarketplaceState>();
            match result {
                Ok(list) => state.set_results(list.items, list.total, list.page),
                Err(e) => state.set_error(format!("Search failed: {e}")),
            }
            cx.refresh_windows();
        })
        .ok();
    })
    .detach();
}

// ── Install / Uninstall ────────────────────────────────────────────────────

/// Install a WASM module from the Hive registry.
pub fn install_extension(
    name: String,
    version: String,
    display_name: String,
    description: String,
    cx: &mut App,
) {
    let registry_url = cx.global::<HiveSettingsModel>().registry_url.clone();
    let token = cx.global::<HiveSettingsModel>().token.clone();
    let mut client = HiveRegistryClient::new(&registry_url);
    if let Some(tok) = token {
        client = client.with_token(tok);
    }

    cx.spawn(async move |cx| {
        match client.download(&name, &version).await {
            Ok(download) => {
                cx.update(|cx| {
                    let extensions = cx.global_mut::<ExtensionsModel>();
                    match install::install_wasm_module(
                        &download,
                        &name,
                        &version,
                        &display_name,
                        &description,
                        extensions,
                    ) {
                        Ok(ext) => {
                            info!(id = %ext.id, "Installed extension from Hive");
                            save_extensions_async(extensions.clone(), cx);
                            // Rescan module directory so the new WASM module
                            // appears in DiscoveredModulesModel and becomes
                            // available as an agent tool. The scan completion
                            // triggers RebuildRequired automatically.
                            module_settings_controller::refresh_runtime(cx);
                        }
                        Err(e) => {
                            error!(error = ?e, "Failed to install extension");
                            let state = cx.global_mut::<MarketplaceState>();
                            state.set_error(format!("Install failed: {e}"));
                        }
                    }
                    cx.refresh_windows();
                })
                .ok();
            }
            Err(chatty_core::hive::ClientError::Unauthorized) => {
                warn!(name = %name, "Download requires authentication");
                cx.update(|cx| {
                    let state = cx.global_mut::<MarketplaceState>();
                    state.set_error(
                        "Login required to download modules. Please sign in first.".to_string(),
                    );
                    cx.refresh_windows();
                })
                .ok();
            }
            Err(e) => {
                error!(error = ?e, name = %name, "Failed to download module");
                cx.update(|cx| {
                    let state = cx.global_mut::<MarketplaceState>();
                    state.set_error(format!("Download failed: {e}"));
                    cx.refresh_windows();
                })
                .ok();
            }
        }
    })
    .detach();
}

/// Uninstall an extension by ID.
pub fn uninstall_extension(id: String, cx: &mut App) {
    let extensions = cx.global_mut::<ExtensionsModel>();
    match install::uninstall_extension(&id, extensions) {
        Ok(()) => {
            info!(id = %id, "Uninstalled extension");
            save_extensions_async(extensions.clone(), cx);
            module_settings_controller::refresh_runtime(cx);
        }
        Err(e) => {
            error!(error = ?e, id = %id, "Failed to uninstall extension");
        }
    }
    cx.refresh_windows();
}

/// Toggle the enabled state of an extension.
///
/// For MCP extensions this also connects/disconnects the MCP service.
/// For A2A extensions this probes the agent card when enabling.
pub fn toggle_extension(id: String, cx: &mut App) {
    let extensions = cx.global_mut::<ExtensionsModel>();
    let Some(ext) = extensions.find_mut(&id) else {
        return;
    };
    ext.enabled = !ext.enabled;
    let is_enabled = ext.enabled;
    let kind = ext.kind.clone();
    info!(id = %id, enabled = is_enabled, "Toggled extension");

    save_extensions_async(extensions.clone(), cx);
    cx.refresh_windows();

    match kind {
        ExtensionKind::McpServer(config) => {
            handle_mcp_toggle(config, is_enabled, cx);
        }
        ExtensionKind::A2aAgent(config) => {
            if is_enabled {
                handle_a2a_probe(id, config, cx);
            }
            emit_rebuild_required(cx);
        }
        ExtensionKind::WasmModule => {
            emit_rebuild_required(cx);
        }
    }
}

/// Handle MCP server connect/disconnect after toggling.
fn handle_mcp_toggle(config: McpServerConfig, is_enabled: bool, cx: &mut App) {
    let service = cx.global::<McpService>().clone();
    let name = config.name.clone();

    if is_enabled {
        cx.global_mut::<ExtensionsModel>()
            .set_mcp_auth_status(name.clone(), McpAuthStatus::Connecting);
        cx.refresh_windows();
    }

    cx.spawn(async move |cx| {
        if is_enabled {
            match service.connect_server(config).await {
                Ok(()) => {
                    cx.update(|cx| {
                        cx.global_mut::<ExtensionsModel>()
                            .set_mcp_auth_status(name.clone(), McpAuthStatus::Authenticated);
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
                        cx.global_mut::<ExtensionsModel>()
                            .set_mcp_auth_status(name.clone(), status);
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
                cx.global_mut::<ExtensionsModel>()
                    .set_mcp_auth_status(name.clone(), McpAuthStatus::NotRequired);
                cx.refresh_windows();
            })
            .ok();
        }

        cx.update(|cx| {
            emit_rebuild_required(cx);
        })
        .ok();
    })
    .detach();
}

/// Probe an A2A agent card after enabling and update cached skills.
fn handle_a2a_probe(
    ext_id: String,
    config: chatty_core::settings::models::a2a_store::A2aAgentConfig,
    cx: &mut App,
) {
    let agent_name = config.name.clone();
    cx.global_mut::<ExtensionsModel>()
        .set_a2a_status(agent_name.clone(), A2aAgentStatus::Connecting);
    cx.refresh_windows();

    let client = A2aClient::new();

    cx.spawn(async move |cx| {
        match client.fetch_agent_card(&config).await {
            Ok(card) => {
                let skills = card.skills.clone();
                cx.update(|cx| {
                    let model = cx.global_mut::<ExtensionsModel>();
                    model.set_a2a_status(agent_name.clone(), A2aAgentStatus::Connected);
                    // Update cached skills in the extension's inner config
                    if let Some(ext) = model.find_mut(&ext_id)
                        && let ExtensionKind::A2aAgent(ref mut cfg) = ext.kind
                    {
                        cfg.skills = skills;
                    }
                    cx.refresh_windows();
                })
                .ok();
                cx.update(|cx| {
                    let model = cx.global::<ExtensionsModel>().clone();
                    save_extensions_async(model, cx);
                })
                .ok();
            }
            Err(e) => {
                let err_msg = format!("{e:#}");
                error!(agent = %agent_name, error = %err_msg, "Failed to fetch A2A agent card");
                cx.update(|cx| {
                    cx.global_mut::<ExtensionsModel>()
                        .set_a2a_status(agent_name, A2aAgentStatus::Failed(err_msg));
                    cx.refresh_windows();
                })
                .ok();
            }
        }
    })
    .detach();
}

/// Add a custom MCP server extension (user-configured, not from Hive).
pub fn add_custom_mcp(name: String, url: String, api_key: Option<String>, cx: &mut App) {
    let config = McpServerConfig {
        name: name.clone(),
        url,
        api_key,
        enabled: true,
        is_module: false,
    };

    // Add to unified extensions store
    let extensions = cx.global_mut::<ExtensionsModel>();
    let id = format!("mcp-{name}");
    extensions.add(InstalledExtension {
        id,
        display_name: name.clone(),
        description: String::new(),
        kind: ExtensionKind::McpServer(config.clone()),
        source: ExtensionSource::Custom,
        enabled: true,
    });
    save_extensions_async(extensions.clone(), cx);

    // Also push into the legacy McpServersModel
    {
        let model = cx.global_mut::<McpServersModel>();
        model.servers_mut().push(config);
        let servers = model.servers().to_vec();
        let repo = chatty_core::mcp_repository();
        cx.spawn(|_cx: &mut AsyncApp| async move {
            if let Err(e) = repo.save_all(servers).await {
                error!(error = ?e, "Failed to save MCP servers");
            }
        })
        .detach();
    }

    cx.refresh_windows();
    info!(server = %name, "Created custom MCP server extension");
}

// ── Hive ↔ MCP token sync ──────────────────────────────────────────────────

/// Propagate the Hive JWT token into the "hive" MCP server's `api_key` so
/// that MCP tool calls include the `Authorization: Bearer <token>` header.
///
/// Pass `None` to clear the token (e.g. on logout).
/// If the server is currently enabled and connected, it is reconnected so
/// the new credentials take effect immediately.
fn sync_hive_token_to_mcp(token: Option<String>, cx: &mut App) {
    let (updated_servers, was_enabled, config) = {
        let model = cx.global_mut::<McpServersModel>();

        let Some(server) = model.servers_mut().iter_mut().find(|s| s.name == "hive") else {
            return;
        };
        let was_enabled = server.enabled;
        server.api_key = token;

        let config = model.servers().iter().find(|s| s.name == "hive").cloned();
        (model.servers().to_vec(), was_enabled, config)
    };

    save_servers_async(updated_servers, cx);

    if was_enabled && let Some(config) = config {
        let service = cx.global::<McpService>().clone();
        let name = config.name.clone();

        cx.spawn(async move |cx| {
                if let Err(e) = service.disconnect_server(&name).await {
                    warn!(server = %name, error = ?e, "Failed to disconnect hive MCP for token update");
                }
                if let Err(e) = service.connect_server(config).await {
                    error!(server = %name, error = ?e, "Failed to reconnect hive MCP with updated token");
                }
                cx.update(|cx| {
                    emit_rebuild_required(cx);
                })
                .ok();
            })
            .detach();
    }
}

// ── Persistence helpers ────────────────────────────────────────────────────

fn save_extensions_async(model: ExtensionsModel, cx: &mut App) {
    cx.spawn(|_cx: &mut AsyncApp| async move {
        let repo = chatty_core::extensions_repository();
        if let Err(e) = repo.save(model).await {
            error!(error = ?e, "Failed to save extensions");
        }
    })
    .detach();
}

fn save_hive_settings_async(model: HiveSettingsModel, cx: &mut App) {
    cx.spawn(|_cx: &mut AsyncApp| async move {
        let repo = chatty_core::hive_settings_repository();
        if let Err(e) = repo.save(model).await {
            error!(error = ?e, "Failed to save Hive settings");
        }
    })
    .detach();
}

fn save_servers_async(servers: Vec<McpServerConfig>, cx: &mut App) {
    cx.spawn(|_cx: &mut AsyncApp| async move {
        let repo = chatty_core::mcp_repository();
        if let Err(e) = repo.save_all(servers).await {
            error!(error = ?e, "Failed to save MCP servers after token sync");
        }
    })
    .detach();
}

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
