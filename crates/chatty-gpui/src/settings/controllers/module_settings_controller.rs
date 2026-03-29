use crate::settings::models::module_settings::{
    ModuleSettingsModel, default_module_dir, normalize_module_dir,
};
use crate::settings::models::{DiscoveredModuleEntry, DiscoveredModulesModel, ModuleLoadStatus};
use anyhow::{Context, Result};
use chatty_module_registry::{ModuleManifest, ModuleRegistry};
use chatty_protocol_gateway::ProtocolGateway;
use chatty_wasm_runtime::{CompletionResponse, LlmProvider, Message, ResourceLimits};
use gpui::{App, AsyncApp};
use std::path::Path;
use std::sync::Arc;
use tracing::{error, info};

struct NoopLlmProvider;

impl LlmProvider for NoopLlmProvider {
    fn complete(
        &self,
        _model: &str,
        _messages: Vec<Message>,
        _tools: Option<String>,
    ) -> Result<CompletionResponse, String> {
        Err("Host LLM integration is not wired for GPUI module runtime yet.".to_string())
    }
}

#[derive(Default)]
struct ScanSnapshot {
    modules: Vec<DiscoveredModuleEntry>,
    scan_error: Option<String>,
}

fn registry_provider() -> Arc<dyn LlmProvider> {
    Arc::new(NoopLlmProvider)
}

fn build_registry(module_dir: &str) -> Result<ModuleRegistry> {
    let mut registry = ModuleRegistry::new(registry_provider(), ResourceLimits::default())
        .context("failed to create module registry")?;
    registry
        .scan_directory(module_dir)
        .with_context(|| format!("failed to scan module directory {module_dir}"))?;
    Ok(registry)
}

fn scan_modules(module_dir: &str) -> ScanSnapshot {
    let root = Path::new(module_dir);
    if !root.exists() {
        return ScanSnapshot {
            modules: Vec::new(),
            scan_error: Some(format!("Module directory does not exist: {module_dir}")),
        };
    }

    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(err) => {
            return ScanSnapshot {
                modules: Vec::new(),
                scan_error: Some(format!(
                    "Failed to read module directory {module_dir}: {err}"
                )),
            };
        }
    };

    let mut validation_registry =
        match ModuleRegistry::new(registry_provider(), ResourceLimits::default()) {
            Ok(registry) => registry,
            Err(err) => {
                return ScanSnapshot {
                    modules: Vec::new(),
                    scan_error: Some(format!("Failed to initialize module runtime: {err}")),
                };
            }
        };

    let mut modules = Vec::new();

    for entry in entries.flatten() {
        let module_dir = entry.path();
        if !module_dir.is_dir() {
            continue;
        }

        let manifest_path = module_dir.join("module.toml");
        if !manifest_path.exists() {
            continue;
        }

        let directory_name = module_dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("unknown")
            .to_string();

        match ModuleManifest::from_file(&manifest_path) {
            Ok(manifest) => {
                let status = match validation_registry.load(&module_dir) {
                    Ok(_) => ModuleLoadStatus::Loaded,
                    Err(err) => ModuleLoadStatus::Error(err.to_string()),
                };

                let wasm_file = manifest
                    .wasm_path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("unknown")
                    .to_string();

                modules.push(DiscoveredModuleEntry {
                    directory_name,
                    name: manifest.name,
                    version: manifest.version,
                    description: manifest.description,
                    wasm_file,
                    tools: manifest.capabilities.tools,
                    chat: manifest.capabilities.chat,
                    agent: manifest.capabilities.agent,
                    openai_compat: manifest.protocols.openai_compat,
                    mcp: manifest.protocols.mcp,
                    a2a: manifest.protocols.a2a,
                    status,
                });
            }
            Err(err) => {
                modules.push(DiscoveredModuleEntry {
                    directory_name: directory_name.clone(),
                    name: directory_name,
                    version: "invalid".to_string(),
                    description: "Manifest could not be parsed.".to_string(),
                    wasm_file: "unknown".to_string(),
                    tools: Vec::new(),
                    chat: false,
                    agent: false,
                    openai_compat: false,
                    mcp: false,
                    a2a: false,
                    status: ModuleLoadStatus::Error(err.to_string()),
                });
            }
        }
    }

    modules.sort_by_cached_key(|module| module.name.to_lowercase());

    ScanSnapshot {
        modules,
        scan_error: None,
    }
}

fn apply_scan_snapshot(
    snapshot: ScanSnapshot,
    settings: &ModuleSettingsModel,
    generation: u64,
    cx: &mut App,
) -> bool {
    {
        let state = cx.global_mut::<DiscoveredModulesModel>();
        if state.refresh_generation != generation {
            return false;
        }

        if let Some(mut gateway) = state.gateway.take() {
            gateway.shutdown();
        }

        state.modules = snapshot.modules;
        state.scan_error = snapshot.scan_error;
        state.scanning = false;
        state.last_scanned_dir = settings.module_dir.clone();
        state.gateway_status = if settings.enabled {
            format!(
                "Starting gateway on http://127.0.0.1:{}",
                settings.gateway_port
            )
        } else {
            "Module runtime disabled".to_string()
        };
    }
    cx.refresh_windows();
    true
}

fn apply_gateway_result(
    settings: &ModuleSettingsModel,
    generation: u64,
    result: Result<ProtocolGateway>,
    cx: &mut App,
) {
    {
        let state = cx.global_mut::<DiscoveredModulesModel>();
        if state.refresh_generation != generation {
            return;
        }

        match result {
            Ok(gateway) => {
                state.gateway_status = format!(
                    "Gateway running on http://127.0.0.1:{}",
                    settings.gateway_port
                );
                state.gateway = Some(gateway);
            }
            Err(err) => {
                state.gateway_status = format!("Gateway failed to start: {err}");
                state.gateway = None;
            }
        }
    }
    cx.refresh_windows();
}

pub fn refresh_runtime(cx: &mut App) {
    let settings = cx.global::<ModuleSettingsModel>().clone();
    let generation = {
        let state = cx.global_mut::<DiscoveredModulesModel>();
        state.refresh_generation += 1;
        state.scanning = true;
        state.last_scanned_dir = settings.module_dir.clone();
        state.scan_error = None;
        state.gateway_status = if settings.enabled {
            format!("Scanning {} and preparing gateway…", settings.module_dir)
        } else {
            format!("Scanning {}…", settings.module_dir)
        };
        if let Some(mut gateway) = state.gateway.take() {
            gateway.shutdown();
        }
        state.refresh_generation
    };
    cx.refresh_windows();

    cx.spawn({
        let settings = settings.clone();
        async move |cx: &mut AsyncApp| {
            let snapshot = tokio::task::spawn_blocking({
                let module_dir = settings.module_dir.clone();
                move || scan_modules(&module_dir)
            })
            .await
            .unwrap_or_else(|err| ScanSnapshot {
                modules: Vec::new(),
                scan_error: Some(format!("Module scan task failed: {err}")),
            });

            let should_start_gateway = cx
                .update(|cx| apply_scan_snapshot(snapshot, &settings, generation, cx))
                .unwrap_or(false)
                && settings.enabled;

            if !should_start_gateway {
                return;
            }

            let registry_result = tokio::task::spawn_blocking({
                let module_dir = settings.module_dir.clone();
                move || build_registry(&module_dir)
            })
            .await
            .unwrap_or_else(|err| Err(anyhow::anyhow!("Module registry task failed: {err}")));

            let gateway_result = match registry_result {
                Ok(registry) => {
                    let shared = Arc::new(tokio::sync::RwLock::new(registry));
                    let mut gateway = ProtocolGateway::new(shared, settings.gateway_port);
                    gateway.start().await.map(|_| gateway)
                }
                Err(err) => Err(err),
            };

            let _ = cx.update(|cx| {
                apply_gateway_result(&settings, generation, gateway_result, cx);
            });
        }
    })
    .detach();
}

/// Persist module settings asynchronously.
fn save_async(cx: &mut App) {
    let settings = cx.global::<ModuleSettingsModel>().clone();
    cx.spawn(|_cx: &mut AsyncApp| async move {
        let repo = chatty_core::module_settings_repository();
        if let Err(e) = repo.save(settings).await {
            error!(error = ?e, "Failed to save module settings");
        }
    })
    .detach();
}

/// Toggle the module runtime on/off.
pub fn toggle_enabled(cx: &mut App) {
    let new_val = !cx.global::<ModuleSettingsModel>().enabled;
    info!(enabled = new_val, "Toggling module runtime");
    cx.global_mut::<ModuleSettingsModel>().enabled = new_val;
    cx.refresh_windows();
    refresh_runtime(cx);
    save_async(cx);
}

/// Update the module directory path.
pub fn set_module_dir(dir: String, cx: &mut App) {
    let dir = normalize_module_dir(dir);
    info!(dir = %dir, "Setting module directory");
    cx.global_mut::<ModuleSettingsModel>().module_dir = dir;
    cx.refresh_windows();
    refresh_runtime(cx);
    save_async(cx);
}

pub fn reset_module_dir(cx: &mut App) {
    set_module_dir(default_module_dir(), cx);
}

/// Update the gateway port.
pub fn set_gateway_port(port: u16, cx: &mut App) {
    info!(port, "Setting gateway port");
    cx.global_mut::<ModuleSettingsModel>().gateway_port = port;
    cx.refresh_windows();
    refresh_runtime(cx);
    save_async(cx);
}
