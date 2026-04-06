use chatty_core::hive::{HiveRegistryClient, models::ListParams};
use chatty_core::install;
use chatty_core::settings::models::extensions_store::{
    ExtensionKind, ExtensionSource, ExtensionsModel, InstalledExtension,
};
use chatty_core::settings::models::hive_settings::HiveSettingsModel;
use chatty_core::settings::models::mcp_store::{McpServerConfig, McpServersModel};
use crate::settings::models::{AgentConfigEvent, GlobalAgentConfigNotifier};
use crate::settings::models::marketplace_state::MarketplaceState;
use gpui::{App, AsyncApp};
use tracing::{error, info, warn};

/// The well-known ID for the built-in Hive MCP extension.
const HIVE_MCP_EXT_ID: &str = "mcp-hive";

// ── Default Hive MCP ──────────────────────────────────────────────────────

/// Ensure the Hive registry MCP server is present in the Extensions store
/// and in McpServersModel. Added on first launch so users can simply enable
/// it once the Hive MCP server is deployed (see hive issue #55).
///
/// Returns `true` if a new entry was added (caller should persist).
pub fn ensure_default_hive_mcp(cx: &mut App) -> bool {
    let extensions = cx.global::<ExtensionsModel>();
    if extensions.is_installed(HIVE_MCP_EXT_ID) {
        return false;
    }

    let registry_url = cx.global::<HiveSettingsModel>().registry_url.clone();
    let mcp_url = format!("{registry_url}/mcp");

    let config = McpServerConfig {
        name: "hive".to_string(),
        url: mcp_url,
        api_key: None,
        enabled: false, // disabled until the MCP endpoint is deployed
        is_module: false,
    };

    // Add to unified Extensions store
    let extensions = cx.global_mut::<ExtensionsModel>();
    extensions.add(InstalledExtension {
        id: HIVE_MCP_EXT_ID.to_string(),
        display_name: "Hive Registry".to_string(),
        description: "Search, browse, and manage Hive modules via MCP.".to_string(),
        kind: ExtensionKind::McpServer(config.clone()),
        source: ExtensionSource::Hive {
            module_name: "hive-mcp".to_string(),
            version: "built-in".to_string(),
        },
        enabled: false,
    });

    // Also add to legacy McpServersModel so McpService knows about it
    let mcp_model = cx.global_mut::<McpServersModel>();
    if !mcp_model.servers().iter().any(|s| s.name == "hive") {
        mcp_model.servers_mut().push(config);
    }

    info!("Added default Hive MCP server extension (disabled)");
    true
}

// ── Authentication ─────────────────────────────────────────────────────────

/// Log in to the Hive registry and persist credentials.
pub fn login(email: String, password: String, cx: &mut App) {
    let registry_url = cx.global::<HiveSettingsModel>().registry_url.clone();
    let client = HiveRegistryClient::new(&registry_url);

    cx.spawn(async move |cx| {
        match client.login(&email, &password).await {
            Ok(auth) => {
                cx.update(|cx| {
                    let settings = cx.global_mut::<HiveSettingsModel>();
                    settings.token = Some(auth.token);
                    settings.username = Some(auth.username.clone());
                    settings.email = Some(email);
                    save_hive_settings_async(settings.clone(), cx);
                    cx.refresh_windows();
                    info!(username = %auth.username, "Logged in to Hive registry");
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
        }
    })
    .detach();
}

/// Register a new account on the Hive registry.
pub fn register(username: String, email: String, password: String, cx: &mut App) {
    let registry_url = cx.global::<HiveSettingsModel>().registry_url.clone();
    let client = HiveRegistryClient::new(&registry_url);

    cx.spawn(async move |cx| {
        match client.register(&username, &email, &password).await {
            Ok(auth) => {
                cx.update(|cx| {
                    let settings = cx.global_mut::<HiveSettingsModel>();
                    settings.token = Some(auth.token);
                    settings.username = Some(auth.username.clone());
                    settings.email = Some(email);
                    save_hive_settings_async(settings.clone(), cx);
                    cx.refresh_windows();
                    info!(username = %auth.username, "Registered with Hive registry");
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
        }
    })
    .detach();
}

/// Log out: clear persisted credentials.
pub fn logout(cx: &mut App) {
    let settings = cx.global_mut::<HiveSettingsModel>();
    settings.token = None;
    settings.username = None;
    settings.email = None;
    save_hive_settings_async(settings.clone(), cx);
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
            client
                .list_modules(&Default::default())
                .await
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

/// Load featured/popular modules for the initial marketplace view.
pub fn load_featured(cx: &mut App) {
    let registry_url = cx.global::<HiveSettingsModel>().registry_url.clone();
    let client = HiveRegistryClient::new(&registry_url);

    cx.spawn(async move |cx| {
        match client
            .list_modules(&ListParams {
                sort: Some("downloads".into()),
                per_page: Some(10),
                ..Default::default()
            })
            .await
        {
            Ok(list) => {
                cx.update(|cx| {
                    let state = cx.global_mut::<MarketplaceState>();
                    state.featured = list.items;
                    cx.refresh_windows();
                })
                .ok();
            }
            Err(e) => {
                warn!(error = ?e, "Failed to load featured modules");
            }
        }
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
    let client = HiveRegistryClient::new(&registry_url);

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
                            emit_rebuild_required(cx);
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
            emit_rebuild_required(cx);
        }
        Err(e) => {
            error!(error = ?e, id = %id, "Failed to uninstall extension");
        }
    }
    cx.refresh_windows();
}

/// Toggle the enabled state of an extension.
pub fn toggle_extension(id: String, cx: &mut App) {
    let extensions = cx.global_mut::<ExtensionsModel>();
    if let Some(ext) = extensions.find_mut(&id) {
        ext.enabled = !ext.enabled;
        info!(id = %id, enabled = ext.enabled, "Toggled extension");
    }
    save_extensions_async(extensions.clone(), cx);
    emit_rebuild_required(cx);
    cx.refresh_windows();
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

    // Also push into the legacy McpServersModel so McpService can connect
    crate::settings::controllers::mcp_controller::create_server(
        name,
        config.url,
        config.api_key,
        cx,
    );
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
