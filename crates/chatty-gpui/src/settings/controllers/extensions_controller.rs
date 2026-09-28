use crate::chatty::services::mcp_service::McpService;
use crate::settings::controllers::module_settings_controller;
use crate::settings::models::marketplace_state::MarketplaceState;
use crate::settings::models::{AgentConfigEvent, GlobalAgentConfigNotifier};
use chatty_core::hive::{HiveRegistryClient, HiveSession, SessionState, TokenPair};
use chatty_core::install;
use chatty_core::services::A2aClient;
use chatty_core::settings::models::a2a_store::A2aAgentStatus;
use chatty_core::settings::models::extensions_store::{
    ExtensionKind, ExtensionSource, ExtensionsModel, InstalledExtension,
};
use chatty_core::settings::models::hive_settings::HiveSettingsModel;
use chatty_core::settings::models::mcp_store::{McpAuthStatus, McpServerConfig, McpServersModel};
use chatty_core::settings::models::module_settings::ModuleSettingsModel;
use gpui::{App, AsyncApp, Global};
use std::path::PathBuf;
use std::sync::Arc;
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

/// Seed entries from the curated external MCP catalog (Notion, etc.) into the
/// Extensions store and McpServersModel. Thin GPUI wrapper around
/// [`chatty_core::install::ensure_curated_mcp_servers`].
///
/// New entries are added disabled so the user must opt in. Returns `true` if
/// at least one new entry was added (caller should persist).
pub fn ensure_curated_mcp_servers(cx: &mut App) -> bool {
    let mut mcp_servers = cx.global::<McpServersModel>().servers().to_vec();

    let extensions = cx.global_mut::<ExtensionsModel>();
    let added = install::ensure_curated_mcp_servers(extensions, &mut mcp_servers);

    if added {
        let mcp_model = cx.global_mut::<McpServersModel>();
        mcp_model.replace_all(mcp_servers);
        info!("Seeded curated MCP catalog entries (disabled)");
    }

    added
}

// ── Authentication ─────────────────────────────────────────────────────────

/// The process's one [`HiveSession`]. Every authenticated Hive client shares
/// it, since two holders of one rotating refresh token would trip the
/// registry's reuse detection and sign the user out.
struct GlobalHiveSession(Arc<HiveSession>);

impl Global for GlobalHiveSession {}

/// The Hive session, once [`install_hive_session`] has run.
pub fn hive_session(cx: &App) -> Option<Arc<HiveSession>> {
    cx.try_global::<GlobalHiveSession>().map(|g| g.0.clone())
}

/// Start the Hive session from the persisted token pair (once; later calls
/// return the running one) and mirror every change it makes — a refreshed
/// pair, a rejected refresh token — back into [`HiveSettingsModel`].
pub fn install_hive_session(cx: &mut App) -> Arc<HiveSession> {
    if let Some(session) = hive_session(cx) {
        return session;
    }
    let settings = cx.global::<HiveSettingsModel>();
    let pair = settings.token_pair();
    let session = Arc::new(HiveSession::new(
        settings.registry_url.clone(),
        pair.clone(),
    ));
    if pair.is_none() && settings.token.is_some() {
        // An access token with no refresh token can't be kept alive: treat
        // the session as ended rather than showing a signed-in user whose
        // calls all 401 within the hour.
        apply_session_state(SessionState::Revoked, cx);
    }
    cx.set_global(GlobalHiveSession(session.clone()));

    let mut states = session.subscribe();
    cx.spawn(async move |cx| {
        while states.changed().await.is_ok() {
            let state = states.borrow_and_update().clone();
            cx.update(|cx| apply_session_state(state, cx))
                .map_err(|e| warn!(error = ?e, "Failed to apply Hive session change"))
                .ok();
        }
    })
    .detach();
    session
}

fn apply_session_state(state: SessionState, cx: &mut App) {
    match state {
        SessionState::SignedIn(pair) => {
            let settings = cx.global_mut::<HiveSettingsModel>();
            if settings.token_pair().as_ref() == Some(&pair) {
                return;
            }
            settings.set_token_pair(Some(&pair));
            save_hive_settings_async(settings.clone(), cx);
        }
        SessionState::Revoked => {
            let settings = cx.global_mut::<HiveSettingsModel>();
            settings.set_token_pair(None);
            settings.username = None;
            save_hive_settings_async(settings.clone(), cx);
            sync_hive_token_to_mcp(None, cx);
            cx.global_mut::<MarketplaceState>().signed_out_of_hive = true;
            cx.refresh_windows();
        }
        SessionState::SignedOut => {}
    }
}

/// Persist a pair from login/registration and hand it to the session.
async fn sign_in(pair: TokenPair, username: String, email: String, cx: &mut AsyncApp) {
    let session = cx.update(|cx| {
        let settings = cx.global_mut::<HiveSettingsModel>();
        settings.set_token_pair(Some(&pair));
        settings.username = Some(username);
        settings.email = Some(email);
        save_hive_settings_async(settings.clone(), cx);
        sync_hive_token_to_mcp(Some(pair.token.clone()), cx);
        cx.global_mut::<MarketplaceState>().signed_out_of_hive = false;
        cx.refresh_windows();
        install_hive_session(cx)
    });
    match session {
        Ok(session) => session.sign_in(pair).await,
        Err(e) => warn!(error = ?e, "Failed to update UI after Hive sign-in"),
    }
}

/// Log in to the Hive registry and persist credentials.
pub fn login(email: String, password: String, cx: &mut App) {
    let registry_url = cx.global::<HiveSettingsModel>().registry_url.clone();
    let client = HiveRegistryClient::new(&registry_url);

    cx.spawn(
        async move |cx| match client.login(&email, &password).await {
            Ok(pair) => {
                let username = pair.username().unwrap_or_default();
                info!(username = %username, "Logged in to Hive registry");
                sign_in(pair, username, email, cx).await;
            }
            Err(e) => {
                error!(error = ?e, "Hive login failed");
                cx.update(|cx| {
                    let state = cx.global_mut::<MarketplaceState>();
                    state.set_error(format!("Login failed: {e}"));
                    cx.refresh_windows();
                })
                .map_err(|e| warn!(error = ?e, "Failed to update UI after login error"))
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
            Ok(pair) => {
                info!(username = %username, "Registered with Hive registry");
                sign_in(pair, username, email, cx).await;
            }
            Err(e) => {
                error!(error = ?e, "Hive registration failed");
                cx.update(|cx| {
                    let state = cx.global_mut::<MarketplaceState>();
                    state.set_error(format!("Registration failed: {e}"));
                    cx.refresh_windows();
                })
                .map_err(|e| warn!(error = ?e, "Failed to update UI after registration error"))
                .ok();
            }
        },
    )
    .detach();
}

/// Log out: clear persisted credentials and revoke the refresh token.
pub fn logout(cx: &mut App) {
    if let Some(session) = hive_session(cx) {
        cx.spawn(async move |_cx: &mut AsyncApp| session.sign_out().await)
            .detach();
    }
    let settings = cx.global_mut::<HiveSettingsModel>();
    settings.set_token_pair(None);
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
        .map_err(|e| warn!(error = ?e, "Failed to update UI after marketplace search"))
        .ok();
    })
    .detach();
}

// ── Install / Uninstall ────────────────────────────────────────────────────

/// The configured module directory (Settings → Modules), where installs,
/// uninstalls and mode switches act.
fn configured_module_dir(cx: &App) -> PathBuf {
    PathBuf::from(&cx.global::<ModuleSettingsModel>().module_dir)
}

/// Install a WASM module (or register a remote module) from the Hive registry
/// into the configured module directory.
///
/// For `execution_mode = "remote"` / `"remote_only"`: fetches version metadata
/// to extract capabilities, writes a `module.toml` marker, and registers in
/// `ExtensionsModel` — no WASM binary is downloaded. Otherwise the `.wasm` is
/// downloaded through [`install::download_wasm_module`] (name and version
/// validated first, body capped while it streams, hash and signature
/// checked) and written with its install record.
pub fn install_extension(
    name: String,
    version: String,
    display_name: String,
    description: String,
    pricing_model: String,
    execution_mode: String,
    cx: &mut App,
) {
    // Refuse a bad registry-supplied name or version before any network or
    // filesystem work (PL-H5a).
    if let Err(e) = install::validate_module_name(&name).and(install::validate_version(&version)) {
        error!(error = %e, "Refusing to install");
        cx.global_mut::<MarketplaceState>()
            .set_error(format!("Install failed: {e}"));
        cx.refresh_windows();
        return;
    }

    let registry_url = cx.global::<HiveSettingsModel>().registry_url.clone();
    let module_dir = configured_module_dir(cx);
    let mut client = HiveRegistryClient::new(&registry_url);
    if let Some(session) = hive_session(cx) {
        client = client.with_session(session);
    }

    let is_remote = matches!(execution_mode.as_str(), "remote" | "remote_only");

    cx.spawn(async move |cx| {
        if is_remote {
            // Fetch version list to get capabilities manifest without downloading WASM.
            let version_manifest = match client.list_versions(&name).await {
                Ok(list) => {
                    list.items
                        .into_iter()
                        .find(|v| v.version == version)
                        .map(|v| v.manifest)
                        .unwrap_or_default()
                }
                Err(e) => {
                    warn!(name = %name, error = ?e, "Failed to fetch version metadata for remote install; using empty manifest");
                    serde_json::Value::Object(Default::default())
                }
            };

            cx.update(|cx| {
                let extensions = cx.global_mut::<ExtensionsModel>();
                match install::install_remote_module(
                    &name,
                    &version,
                    &display_name,
                    &description,
                    &pricing_model,
                    &version_manifest,
                    &module_dir,
                    extensions,
                ) {
                    Ok(ext) => {
                        info!(id = %ext.id, "Installed remote extension from Hive");
                        save_extensions_async(extensions.clone(), cx);
                        module_settings_controller::refresh_runtime(cx);
                    }
                    Err(e) => {
                        error!(error = ?e, "Failed to install remote extension");
                        let state = cx.global_mut::<MarketplaceState>();
                        state.set_error(format!("Install failed: {e}"));
                    }
                }
                cx.refresh_windows();
            })
            .map_err(|e| warn!(error = ?e, "Failed to update UI after remote install"))
            .ok();
        } else {
            // Signal download start so the UI can show the progress bar.
            cx.update(|cx| {
                cx.global_mut::<MarketplaceState>().set_download_progress(&name, 0.0);
                cx.refresh_windows();
            })
            .map_err(|e| warn!(error = ?e, "Failed to signal download start in UI"))
            .ok();

            let downloaded = install::download_wasm_module(&client, &name, &version, |read, total| {
                if total > 0 {
                    let progress = (read as f32 / total as f32).min(0.99);
                    cx.update(|cx| {
                        cx.global_mut::<MarketplaceState>()
                            .set_download_progress(&name, progress);
                        cx.refresh_windows();
                    })
                    .map_err(|e| warn!(error = ?e, "Failed to update download progress in UI"))
                    .ok();
                }
            })
            .await;

            cx.update(|cx| {
                cx.global_mut::<MarketplaceState>().clear_download_progress(&name);
                let download = match downloaded {
                    Ok(download) => download,
                    Err(install::InstallError::Client(
                        chatty_core::hive::ClientError::Unauthorized,
                    )) => {
                        warn!(name = %name, "Download requires authentication");
                        cx.global_mut::<MarketplaceState>().set_error(
                            "Login required to download modules. Please sign in first.".to_string(),
                        );
                        cx.refresh_windows();
                        return;
                    }
                    Err(e) => {
                        error!(error = ?e, name = %name, "Module download failed");
                        cx.global_mut::<MarketplaceState>()
                            .set_error(format!("Download failed: {e}"));
                        cx.refresh_windows();
                        return;
                    }
                };

                let extensions = cx.global_mut::<ExtensionsModel>();
                match install::install_wasm_module(
                    &download,
                    &name,
                    &version,
                    &display_name,
                    &description,
                    &pricing_model,
                    &module_dir,
                    extensions,
                ) {
                    Ok(ext) => {
                        info!(id = %ext.id, "Installed extension from Hive");
                        save_extensions_async(extensions.clone(), cx);
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
            .map_err(|e| warn!(error = ?e, "Failed to update UI after install"))
            .ok();
        }
    })
    .detach();
}

/// Uninstall an extension by ID (a WASM module's files go from the
/// configured module directory).
pub fn uninstall_extension(id: String, cx: &mut App) {
    let module_dir = configured_module_dir(cx);
    let extensions = cx.global_mut::<ExtensionsModel>();
    match install::uninstall_extension(&id, &module_dir, extensions) {
        Ok(()) => {
            info!(id = %id, "Uninstalled extension");
            save_extensions_async(extensions.clone(), cx);
            module_settings_controller::refresh_runtime(cx);
        }
        Err(e) => {
            error!(error = ?e, id = %id, "Failed to uninstall extension");
            let state = cx.global_mut::<MarketplaceState>();
            state.set_error(format!("Uninstall failed: {e}"));
        }
    }
    cx.refresh_windows();
}

/// Switch a WASM module's execution mode between `"local"` and `"remote"`.
/// After updating `module.toml` on disk, triggers a module re-scan so the new
/// mode is reflected immediately in the UI.
pub fn set_execution_mode(id: String, mode: String, cx: &mut App) {
    match install::set_module_execution_mode(&id, &mode, &configured_module_dir(cx)) {
        Ok(()) => {
            info!(id = %id, mode = %mode, "Updated module execution mode");
            module_settings_controller::refresh_runtime(cx);
        }
        Err(e) => {
            error!(error = ?e, id = %id, mode = %mode, "Failed to set execution mode");
            let state = cx.global_mut::<MarketplaceState>();
            state.set_error(format!("Could not change execution mode: {e}"));
        }
    }
    cx.refresh_windows();
}

/// Toggle the enabled state of an extension.
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
                    .map_err(|e| warn!(error = ?e, "Failed to update UI after MCP connect"))
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
                    .map_err(|e| warn!(error = ?e, "Failed to update UI after MCP connect error"))
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
            .map_err(|e| warn!(error = ?e, "Failed to update UI after MCP disconnect"))
            .ok();
        }

        cx.update(|cx| {
            emit_rebuild_required(cx);
        })
        .map_err(|e| warn!(error = ?e, "Failed to emit rebuild after MCP toggle"))
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
                .map_err(|e| warn!(error = ?e, "Failed to update UI after A2A probe"))
                .ok();
                cx.update(|cx| {
                    let model = cx.global::<ExtensionsModel>().clone();
                    save_extensions_async(model, cx);
                })
                .map_err(|e| warn!(error = ?e, "Failed to persist extensions after A2A probe"))
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
                .map_err(|e| warn!(error = ?e, "Failed to update UI after A2A probe error"))
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
        pricing_model: None,
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
                .map_err(|e| warn!(error = ?e, "Failed to emit rebuild-required after hive token update"))
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
    if let Some(notifier) = cx
        .try_global::<GlobalAgentConfigNotifier>()
        .and_then(|g| g.try_upgrade())
    {
        notifier.update(cx, |_notifier, cx| {
            cx.emit(AgentConfigEvent::RebuildRequired);
        });
    }
}
