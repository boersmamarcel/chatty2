#[cfg(unix)]
use crate::chatty::services::broker_runner;
use crate::chatty::services::lazy_gateway_broker::{self, LazyGatewayBroker};
use crate::settings::models::module_settings::ModuleSettingsModel;
use crate::settings::models::{
    AgentConfigEvent, DiscoveredModuleEntry, DiscoveredModulesModel, GlobalAgentConfigNotifier,
    ModuleLoadStatus,
};
use anyhow::{Context, Result};
#[cfg(unix)]
use chatty_core::agent_spec::load_roster;
use chatty_core::hive::{CreditGuard, HiveRegistryClient, UsageCollector, UsageCollectorConfig};
use chatty_core::services::plugin_llm::PluginLlmProvider;
#[cfg(unix)]
use chatty_core::services::virtual_agents::resolve_virtual_agents;
use chatty_core::settings::models::execution_settings::{ApprovalMode, ExecutionSettingsModel};
use chatty_core::settings::models::extensions_store::{
    ExtensionKind, ExtensionSource, ExtensionsModel,
};
use chatty_core::settings::models::hive_settings::HiveSettingsModel;
use chatty_core::settings::models::models_store::{ModelsModel, resolve_model_query};
use chatty_core::settings::models::providers_store::ProviderModel;
use chatty_module_registry::{ModuleManifest, ModuleRegistry};
use chatty_protocol_gateway::ProtocolGateway;
use chatty_wasm_runtime::{CompletionResponse, LlmProvider, Message, ResourceLimits};
use gpui::{App, AsyncApp};
use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use tracing::{error, info, warn};

/// The provider a WASM module's `llm::complete()` reaches, built from the
/// current GPUI globals: chatty-core's [`PluginLlmProvider`], over the same
/// provider clients agents use (PL-H2, AGE-605). The module registry is
/// shared by every conversation, so the calling agent's model is the one
/// conversations start with (`resolve_model_query` with no query: the
/// default model, else the roster's first). Returns `None` if no model or no
/// matching provider is configured, or no Tokio runtime is entered to drive
/// the requests.
fn build_llm_provider(cx: &App) -> Option<Arc<dyn LlmProvider>> {
    let models = cx.try_global::<ModelsModel>()?.models();
    let providers = cx.try_global::<ProviderModel>()?.providers();

    let runtime = tokio::runtime::Handle::try_current().ok()?;
    let calling_model = resolve_model_query(models, None)?;
    if !providers
        .iter()
        .any(|p| p.provider_type == calling_model.provider_type)
    {
        return None;
    }

    info!(
        provider = ?calling_model.provider_type,
        model = %calling_model.model_identifier,
        "Building the plugin LLM provider for WASM modules"
    );

    Some(Arc::new(PluginLlmProvider::new(
        calling_model.clone(),
        models.to_vec(),
        providers.to_vec(),
        runtime,
    )))
}

#[derive(Default)]
struct ScanSnapshot {
    modules: Vec<DiscoveredModuleEntry>,
    scan_error: Option<String>,
}

fn build_registry(module_dir: &str, llm_provider: Arc<dyn LlmProvider>) -> Result<ModuleRegistry> {
    let mut registry = ModuleRegistry::new(llm_provider, ResourceLimits::default())
        .context("failed to create module registry")?;
    registry
        .scan_directory(module_dir)
        .with_context(|| format!("failed to scan module directory {module_dir}"))?;
    Ok(registry)
}

/// Noop provider used only for module validation during scanning
/// (modules are loaded but not executed, so llm::complete is never called).
fn noop_provider() -> Arc<dyn LlmProvider> {
    struct Noop;
    impl LlmProvider for Noop {
        fn complete(
            &self,
            _model: &str,
            _messages: Vec<Message>,
            _tools: Option<String>,
        ) -> Result<CompletionResponse, String> {
            Err("LLM not available in validation context".to_string())
        }
    }
    Arc::new(Noop)
}

fn scan_modules(module_dir: &str) -> ScanSnapshot {
    let root = Path::new(module_dir);
    if !root.exists() {
        return ScanSnapshot {
            modules: Vec::new(),
            scan_error: Some(format!("Module directory does not exist: {module_dir}")),
        };
    }

    let mut validation_registry =
        match ModuleRegistry::new(noop_provider(), ResourceLimits::default()) {
            Ok(registry) => registry,
            Err(err) => {
                return ScanSnapshot {
                    modules: Vec::new(),
                    scan_error: Some(format!("Failed to initialize module runtime: {err}")),
                };
            }
        };

    // The registry's scan is the one source of each entry's status: what it
    // loaded, what is remote, and every failure with its reason (PL-H3).
    let report = match validation_registry.scan_directory(root) {
        Ok(report) => report,
        Err(err) => {
            return ScanSnapshot {
                modules: Vec::new(),
                scan_error: Some(format!(
                    "Failed to read module directory {module_dir}: {err:#}"
                )),
            };
        }
    };

    let loaded = report.loaded.into_iter().map(|(dir, manifest)| {
        let trust_level = validation_registry.trust_level(&manifest.name);
        let requested = validation_registry
            .requested_capabilities(&manifest.name)
            .map(|caps| caps.iter().map(|c| c.name().to_string()).collect());
        DiscoveredModuleEntry {
            trust_level,
            requested,
            ..discovered_entry(&dir, manifest, ModuleLoadStatus::Loaded)
        }
    });
    let remote = report
        .remote
        .into_iter()
        .map(|(dir, manifest)| discovered_entry(&dir, manifest, ModuleLoadStatus::Remote));
    let failed = report.failed.into_iter().map(|(dir, reason)| {
        let status = ModuleLoadStatus::Error(reason);
        // A manifest that parses (the failure was its `.wasm` or a duplicate
        // name) still names the module; otherwise the directory stands in.
        match ModuleManifest::from_file(&dir.join("module.toml")) {
            Ok(manifest) => discovered_entry(&dir, manifest, status),
            Err(_) => invalid_manifest_entry(&dir, status),
        }
    });
    let mut modules: Vec<DiscoveredModuleEntry> = loaded.chain(remote).chain(failed).collect();

    modules.sort_by_cached_key(|module| module.name.to_lowercase());

    ScanSnapshot {
        modules,
        scan_error: None,
    }
}

fn directory_name(dir: &Path) -> String {
    dir.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("unknown")
        .to_string()
}

fn discovered_entry(
    dir: &Path,
    manifest: ModuleManifest,
    status: ModuleLoadStatus,
) -> DiscoveredModuleEntry {
    let wasm_file = if manifest.execution_mode.is_remote() {
        "remote".to_string()
    } else {
        manifest
            .wasm_path
            .as_ref()
            .and_then(|p| p.file_name())
            .and_then(|name| name.to_str())
            .unwrap_or("unknown")
            .to_string()
    };
    DiscoveredModuleEntry {
        directory_name: directory_name(dir),
        name: manifest.name,
        version: manifest.version,
        description: manifest.description,
        wasm_file,
        tools: manifest.capabilities.tools,
        mcp: manifest.protocols.mcp,
        status,
        execution_mode: manifest.execution_mode.to_string(),
        trust_level: None,
        requested: None,
    }
}

fn invalid_manifest_entry(dir: &Path, status: ModuleLoadStatus) -> DiscoveredModuleEntry {
    let directory_name = directory_name(dir);
    DiscoveredModuleEntry {
        directory_name: directory_name.clone(),
        name: directory_name,
        version: "invalid".to_string(),
        description: "Manifest could not be parsed.".to_string(),
        wasm_file: "unknown".to_string(),
        tools: Vec::new(),
        mcp: false,
        status,
        execution_mode: "local".to_string(),
        trust_level: None,
        requested: None,
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
            "Starting gateway".to_string()
        } else {
            "Module runtime disabled".to_string()
        };
    }
    cx.refresh_windows();

    // Notify the active agent to rebuild so it picks up newly discovered
    // (or removed) plugins and the broker's roster.
    if let Some(notifier) = cx
        .try_global::<GlobalAgentConfigNotifier>()
        .and_then(|g| g.try_upgrade())
    {
        notifier.update(cx, |_notifier, cx| {
            cx.emit(AgentConfigEvent::RebuildRequired);
        });
    }

    true
}

/// Where an external MCP client finds the running gateway: its socket and
/// the `0600` file holding its launch token (ADR-0021 § 4).
fn gateway_running_status(gateway: &ProtocolGateway) -> String {
    match (gateway.socket_path(), gateway.token_path()) {
        (Some(socket), Some(token)) => format!(
            "Gateway running on {} (token in {})",
            socket.display(),
            token.display()
        ),
        _ => "Gateway running".to_string(),
    }
}

fn apply_gateway_result(
    generation: u64,
    result: Result<ProtocolGateway>,
    gateway_workspace: Option<std::path::PathBuf>,
    cx: &mut App,
) {
    // A module's tools reach an agent as a plugin its spec lists (PL-U2),
    // not as an MCP server pointed at this gateway; `/mcp/{module}` stays
    // for external MCP clients.
    let started = {
        let state = cx.global_mut::<DiscoveredModulesModel>();
        if state.refresh_generation != generation {
            return;
        }

        match result {
            Ok(gateway) => {
                state.gateway_status = gateway_running_status(&gateway);
                state.gateway = Some(gateway);
                state.gateway_workspace = gateway_workspace;
                true
            }
            Err(err) => {
                state.gateway_status = format!("Gateway failed to start: {err}");
                state.gateway = None;
                state.gateway_workspace = None;
                false
            }
        }
    };

    cx.refresh_windows();

    // The gateway (and thus the roster it actually serves, AGE-719) just
    // became live — later than `apply_scan_snapshot`'s own
    // `RebuildRequired`, which fires before the gateway exists. Rebuild the
    // active conversation's agent now so its `local_agents` picks up the
    // roster this gateway was actually built for, rather than staying
    // pinned to whatever it guessed before the gateway existed.
    if started
        && let Some(notifier) = cx
            .try_global::<GlobalAgentConfigNotifier>()
            .and_then(|g| g.try_upgrade())
    {
        notifier.update(cx, |_notifier, cx| {
            cx.emit(AgentConfigEvent::RebuildRequired);
        });
    }
}

/// Toggle the WASM module runtime on/off (Settings → Plugins), persist to
/// disk, and start or stop the gateway/broker immediately via
/// `refresh_runtime` — the same reload path a settings-file edit gets
/// after a restart, run live instead so the tutorial's "quit and restart
/// Chatty" step is no longer needed.
pub fn toggle_module_runtime(cx: &mut App) {
    let old_enabled = cx.global::<ModuleSettingsModel>().enabled;
    let new_enabled = !old_enabled;
    info!(
        old = old_enabled,
        new = new_enabled,
        "Toggling module runtime"
    );
    cx.global_mut::<ModuleSettingsModel>().enabled = new_enabled;

    let settings = cx.global::<ModuleSettingsModel>().clone();
    cx.refresh_windows();

    refresh_runtime(cx);

    cx.spawn(|_cx: &mut AsyncApp| async move {
        let repo = chatty_core::module_settings_repository();
        if let Err(e) = repo.save(settings).await {
            error!(error = ?e, "Failed to save module settings");
        }
    })
    .detach();
}

pub fn refresh_runtime(cx: &mut App) {
    let settings = cx.global::<ModuleSettingsModel>().clone();
    let llm_provider = build_llm_provider(cx).unwrap_or_else(|| {
        warn!("No LLM provider configured; WASM modules will not be able to call llm::complete()");
        noop_provider()
    });
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
        // A pending broker from an earlier refresh is replaced outright
        // (BI-2, AGE-634): if it never started, there was nothing bound to
        // tear down; if it did, `state.gateway` above already shut it down.
        state.lazy_broker = None;
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

            // The gateway — and the broker riding on it (ADR-0011 C2) —
            // starts on the first `list_agents`/`invoke_agent` call, not
            // here (BI-2, AGE-634). Publish a `LazyGatewayBroker` that asks
            // *this* task to do the work when that first call comes in:
            // building and starting the gateway needs `cx` (`AsyncApp`),
            // which is `!Send`, so it cannot happen on whatever tokio thread
            // the tool call runs on. This task stays parked on
            // `request_rx.recv()` until then, or exits with nothing bound if
            // settings change again first (dropping the sender).
            let (request_tx, mut request_rx) = tokio::sync::mpsc::unbounded_channel();
            let broker = Arc::new(LazyGatewayBroker::new(request_tx));
            let published = cx
                .update(|cx| {
                    let state = cx.global_mut::<DiscoveredModulesModel>();
                    if state.refresh_generation != generation {
                        return false;
                    }
                    state.gateway_status =
                        "Gateway will start on the first delegation".to_string();
                    state.lazy_broker = Some(broker);
                    true
                })
                .unwrap_or(false);
            if !published {
                return;
            }

            let Some(reply) = request_rx.recv().await else {
                // A later refresh replaced this broker before anything ever
                // asked for it (dropping `request_tx`): nothing to start.
                return;
            };

            let registry_result = tokio::task::spawn_blocking({
                let module_dir = settings.module_dir.clone();
                let provider = llm_provider.clone();
                move || build_registry(&module_dir, provider)
            })
            .await
            .unwrap_or_else(|err| Err(anyhow::anyhow!("Module registry task failed: {err}")));

            // The workspace this gateway's roster actually ends up serving
            // (AGE-719); stays `None` on non-unix, where there are no
            // virtual-agent runners to resolve one for.
            #[cfg_attr(not(unix), allow(unused_mut))]
            let mut gateway_workspace: Option<std::path::PathBuf> = None;
            // What `LazyGatewayBroker` hands `invoke_agent`: the socket, and
            // the root's direct handle into this broker (AGE-744).
            let mut started = None;
            let gateway_result = match registry_result {
                Ok(registry) => {
                    let shared = Arc::new(tokio::sync::RwLock::new(registry));
                    let mut gateway = ProtocolGateway::new(shared);

                    // Attach hive client and runner URL for remote execution support
                    let hive_settings_result = cx.update(|cx| {
                        let hive = cx.global::<HiveSettingsModel>();
                        let ext = cx.global::<ExtensionsModel>();
                        // Collect module names of paid Hive WASM modules
                        let paid_modules: HashSet<String> = ext
                            .extensions
                            .iter()
                            .filter(|e| {
                                matches!(e.kind, ExtensionKind::WasmModule)
                                    && matches!(e.pricing_model.as_deref(), Some("paid"))
                            })
                            .filter_map(|e| match &e.source {
                                ExtensionSource::Hive { module_name, .. } => {
                                    Some(module_name.clone())
                                }
                                _ => None,
                            })
                            .collect();
                        let session = super::extensions_controller::hive_session(cx);
                        (hive.clone(), session, paid_modules)
                    });

                    if let Ok((hive_settings, session, paid_modules)) = hive_settings_result {
                        // Authenticate as the signed-in user, if any
                        let mut hive_client = HiveRegistryClient::new(&hive_settings.registry_url);
                        if let Some(ref session) = session {
                            hive_client = hive_client.with_session(Arc::clone(session));
                        }
                        let hive_client = Arc::new(hive_client);

                        // Wire CreditGuard for pre-invocation balance checks
                        let credit_guard =
                            Arc::new(CreditGuard::with_default_ttl(Arc::clone(&hive_client)));

                        // One `UsageCollector` per process (PL-H9, AGE-612):
                        // `refresh_runtime` runs on every settings change, so
                        // a fresh collector here would also leak a fresh
                        // background flush task racing every earlier one
                        // over the same queue file. `global` reuses the
                        // process's one instance instead.
                        let usage_config = UsageCollectorConfig::default();
                        let usage_collector =
                            UsageCollector::global(&hive_settings.registry_url, usage_config);
                        if let Some(session) = session {
                            let uc = Arc::clone(&usage_collector);
                            tokio::spawn(async move { uc.set_session(session).await });
                        }
                        // Start the periodic flush task — idempotent, so
                        // this is a no-op after the first call. Without it
                        // (ever), events accumulate in memory but are never
                        // sent to the registry, so dashboard analytics never
                        // increment.
                        usage_collector.start_background_flush();

                        gateway = gateway
                            .with_hive_client(hive_client)
                            .with_runner_url(hive_settings.runner_url)
                            .with_credit_guard(credit_guard)
                            .with_usage_collector(usage_collector)
                            .with_paid_modules(paid_modules);
                    }

                    // ADR-0011 C2: the gateway is also the fleet broker.
                    // Each virtual agent — the roster's specs: what module
                    // settings declare, else `local-agent` and every
                    // exposed spec (C10, PL-U5) — spawns one child per
                    // delegated task, on a connection the broker makes for
                    // it (ADR-0020); the shared socket beside the gateway's
                    // refuses every registration. Unix only — the runners do
                    // not exist on Windows, so the gateway there is just the
                    // gateway.
                    #[cfg(unix)]
                    {
                        let participants = gateway.participants();
                        broker_runner::serve_socket(broker_runner::socket_path());
                        let (resolved_workspace, specs) = cx
                            .update(|cx| {
                                let exec = cx.global::<ExecutionSettingsModel>();
                                // A worker inherits the desktop's approval
                                // policy; it has no user to ask. A desktop
                                // leader forwards no provider flags: the
                                // child reads the same config dir.
                                let common_args: Vec<String> = if matches!(
                                    exec.approval_mode,
                                    ApprovalMode::AutoApproveAll
                                ) {
                                    vec!["--auto-approve".to_string()]
                                } else {
                                    Vec::new()
                                };
                                let models = cx
                                    .try_global::<ModelsModel>()
                                    .map(|m| m.models())
                                    .unwrap_or(&[]);
                                let providers = cx
                                    .try_global::<ProviderModel>()
                                    .map(|p| p.providers())
                                    .unwrap_or(&[]);
                                // The same workspace a conversation's own
                                // `local_agents` resolves to
                                // (`gateway_and_roster`, AGE-719): the
                                // active conversation's own working
                                // directory when it has one, else the
                                // shared default.
                                let active_conv_workspace = cx
                                    .try_global::<crate::chatty::models::ConversationsStore>()
                                    .and_then(|store| {
                                        store
                                            .active_id()
                                            .and_then(|id| store.get_conversation(id))
                                    })
                                    .and_then(|conv| conv.working_dir())
                                    .cloned();
                                let resolved_workspace = chatty_core::agent_spec::roster_workspace(
                                    exec.workspace_dir.as_deref().map(Path::new),
                                    active_conv_workspace.as_deref(),
                                )
                                .map(Path::to_path_buf);
                                // The roster's specs, looked up from the
                                // workspace a worker's tree comes from. A
                                // spec that does not load leaves the
                                // broker without workers, loudly.
                                let agents = match load_roster(
                                    &settings.virtual_agents,
                                    resolved_workspace.as_deref(),
                                ) {
                                    Ok(agents) => agents,
                                    Err(e) => {
                                        error!(error = ?e, "Failed to load the virtual agents' specs");
                                        return (resolved_workspace, Vec::new());
                                    }
                                };
                                let specs = resolve_virtual_agents(
                                    models,
                                    providers,
                                    &settings,
                                    &agents,
                                    &common_args,
                                    resolved_workspace.as_deref(),
                                );
                                (resolved_workspace, specs)
                            })
                            .unwrap_or((None, Vec::new()));
                        gateway_workspace = resolved_workspace.clone();
                        // A node's call is checked against the roster's
                        // specs before anything is spawned (PL-S2). The
                        // root is the desktop's conversation, which is not
                        // built from a spec, so it may call anyone
                        // (AGE-745).
                        gateway = gateway.with_call_policy(Arc::new(
                            chatty_core::services::delegation_policy::SpecPolicy::for_agents(
                                &specs,
                            ),
                        ));
                        for runner in broker_runner::local_runners(
                            participants,
                            resolved_workspace.map(|dir| dir.to_string_lossy().to_string()),
                            settings.default_endpoint_budget,
                            specs,
                        ) {
                            gateway = gateway.with_virtual_agent(Arc::new(runner));
                        }
                    }

                    lazy_gateway_broker::start(&mut gateway)
                        .await
                        .map(|gateway_started| {
                            started = Some(gateway_started);
                            gateway
                        })
                }
                Err(err) => Err(err),
            };

            let error_text = gateway_result.as_ref().err().map(|e| e.to_string());
            let _ = cx.update(|cx| {
                apply_gateway_result(generation, gateway_result, gateway_workspace, cx);
            });
            let _ = reply.send(started.ok_or_else(|| {
                error_text.unwrap_or_else(|| "the gateway did not start".to_string())
            }));
        }
    })
    .detach();
}

#[cfg(test)]
mod refresh_runtime_tests {
    use super::*;
    use gpui::TestAppContext;

    fn module_entry(name: &str, mcp: bool) -> DiscoveredModuleEntry {
        DiscoveredModuleEntry {
            directory_name: name.to_string(),
            name: name.to_string(),
            version: "0.1.0".to_string(),
            description: String::new(),
            wasm_file: format!("{name}.wasm"),
            tools: Vec::new(),
            mcp,
            status: ModuleLoadStatus::Loaded,
            execution_mode: "local".to_string(),
            trust_level: None,
            requested: None,
        }
    }

    /// 4.4: two `refresh_runtime` calls fired back to back (as a rapid
    /// settings edit would) must end with exactly one gateway — not two
    /// racing generations.
    ///
    /// The full async assembly (`spawn_blocking` scan -> lazy broker ->
    /// gateway bind -> usage-flush start) needs a real Tokio runtime
    /// underneath gpui's own executor to drive to completion; `gpui::test`'s
    /// `TestAppContext` does not provide one (confirmed empirically: even
    /// with a leaked, entered `Runtime` on this thread, `run_until_parked`
    /// reports nothing scheduled for an `App`-level `cx.spawn`, and the
    /// spawned task never observably progresses). That is a test-harness
    /// gap, not a `refresh_runtime` bug, and is exactly why the plan's own
    /// S4 rows 4.9/4.10 exercise the desktop for real (scripted, over a
    /// running gateway) rather than through a unit test.
    ///
    /// What *is* directly testable, without any of that machinery, is the
    /// synchronous guard every later step depends on:
    /// `apply_scan_snapshot` refuses to apply a stale generation's result.
    /// If two `refresh_runtime` calls race, the first one's scan (however
    /// long it takes) can never clobber state the second one already
    /// started — which is the actual mechanism that makes "two rapid
    /// refreshes -> one gateway" true. The gateway-bind and usage-flush
    /// halves of F13 are PL-H9 (AGE-612)'s own tests.
    #[gpui::test]
    fn row_4_4_stale_generation_is_refused(cx: &mut TestAppContext) {
        cx.update(|cx| {
            cx.set_global(DiscoveredModulesModel {
                refresh_generation: 2,
                ..Default::default()
            });
            let settings = ModuleSettingsModel::default();

            // Generation 1's scan finishes after generation 2 already
            // started (the exact race two rapid `refresh_runtime` calls
            // create): it must be discarded, not applied.
            let stale = ScanSnapshot {
                modules: vec![module_entry("stale-module", false)],
                scan_error: None,
            };
            let applied = apply_scan_snapshot(stale, &settings, 1, cx);
            assert!(!applied, "a stale generation's scan must be refused");
            assert!(
                cx.global::<DiscoveredModulesModel>().modules.is_empty(),
                "the stale scan's modules must never reach the global state"
            );

            // Generation 2's own (current) scan is applied normally.
            let current = ScanSnapshot {
                modules: vec![module_entry("current-module", false)],
                scan_error: None,
            };
            let applied = apply_scan_snapshot(current, &settings, 2, cx);
            assert!(applied, "the current generation's scan must be applied");
            assert_eq!(
                cx.global::<DiscoveredModulesModel>()
                    .modules
                    .iter()
                    .map(|m| m.name.as_str())
                    .collect::<Vec<_>>(),
                vec!["current-module"],
                "only the current generation's modules should be visible, \
                 never a mix with a superseded scan's"
            );
        });
    }
}
