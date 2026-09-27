#[cfg(unix)]
use crate::chatty::services::broker_runner;
use crate::chatty::services::lazy_gateway_broker::LazyGatewayBroker;
use crate::settings::models::mcp_store::{McpServerConfig, McpServersModel};
use crate::settings::models::module_settings::ModuleSettingsModel;
use crate::settings::models::{
    AgentConfigEvent, DiscoveredModuleEntry, DiscoveredModulesModel, GlobalAgentConfigNotifier,
    ModuleLoadStatus,
};
use anyhow::{Context, Result};
#[cfg(unix)]
use chatty_core::agent_spec::load_roster;
use chatty_core::hive::{CreditGuard, HiveRegistryClient, UsageCollector, UsageCollectorConfig};
#[cfg(unix)]
use chatty_core::services::virtual_agents::resolve_virtual_agents;
use chatty_core::settings::models::execution_settings::{ApprovalMode, ExecutionSettingsModel};
use chatty_core::settings::models::extensions_store::{
    ExtensionKind, ExtensionSource, ExtensionsModel,
};
use chatty_core::settings::models::hive_settings::HiveSettingsModel;
use chatty_core::settings::models::models_store::ModelsModel;
use chatty_core::settings::models::providers_store::{ProviderModel, ProviderType};
use chatty_module_registry::{ModuleManifest, ModuleRegistry};
use chatty_protocol_gateway::ProtocolGateway;
use chatty_wasm_runtime::{
    CompletionResponse, LlmProvider, Message, ResourceLimits, Role, TokenUsage, ToolCall,
};
use gpui::{App, AsyncApp};
use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use tracing::{debug, error, info, warn};

// ---------------------------------------------------------------------------
// HostLlmProvider — bridges WASM module llm::complete() to real LLM APIs
// ---------------------------------------------------------------------------

/// Configuration captured from GPUI globals for the LLM provider.
#[derive(Clone, Debug)]
struct LlmConfig {
    provider_type: ProviderType,
    api_key: Option<String>,
    base_url: Option<String>,
    model_identifier: String,
    temperature: f32,
    max_tokens: Option<i32>,
}

struct HostLlmProvider {
    config: LlmConfig,
    client: reqwest::Client,
}

impl HostLlmProvider {
    fn new(config: LlmConfig) -> Self {
        let client = chatty_core::services::http_client::default_client(120);
        Self { config, client }
    }

    fn effective_model(&self, model: &str) -> String {
        if model.is_empty() {
            self.config.model_identifier.clone()
        } else {
            model.to_string()
        }
    }

    fn role_str(role: &Role) -> &'static str {
        match role {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
        }
    }

    /// Normalize a JSON tools blob from a WASM module into a canonical list
    /// of `{name, description, parameters}` records.
    ///
    /// Accepts either:
    /// - OpenAI wrapped form: `[{"type":"function","function":{name,description,parameters}}]`
    /// - Flat form: `[{name, description, parameters}]` (or `parameters_schema`)
    /// - A single object instead of an array
    ///
    /// Tools missing a `name` are dropped.
    fn normalize_tools(tools_json: &str) -> Vec<serde_json::Value> {
        let parsed: serde_json::Value = match serde_json::from_str(tools_json) {
            Ok(v) => v,
            Err(_) => return Vec::new(),
        };
        let arr: Vec<serde_json::Value> = match parsed {
            serde_json::Value::Array(a) => a,
            v @ serde_json::Value::Object(_) => vec![v],
            _ => return Vec::new(),
        };
        arr.into_iter()
            .filter_map(|t| {
                let inner = t.get("function").cloned().unwrap_or(t);
                let name = inner.get("name")?.as_str()?.to_string();
                let description = inner
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let parameters = inner
                    .get("parameters")
                    .or_else(|| inner.get("parameters_schema"))
                    .or_else(|| inner.get("input_schema"))
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({"type": "object"}));
                Some(serde_json::json!({
                    "name": name,
                    "description": description,
                    "parameters": parameters,
                }))
            })
            .collect()
    }

    /// Build the (unsent) request for one `llm::complete` call: the URL,
    /// headers and JSON body that `complete_openai` would send. Split out
    /// so PL-E7's row 4.1 (AGE-602) can inspect the built `reqwest::Request`
    /// (`.build()`) instead of sending it — no test may reach the internet,
    /// and the "OpenRouter default" and bare "Ollama" cases hit a hardcoded
    /// host with no override.
    fn build_request(
        &self,
        model: &str,
        messages: &[Message],
        tools: &Option<String>,
    ) -> reqwest::RequestBuilder {
        let base = self
            .config
            .base_url
            .as_deref()
            .unwrap_or(match self.config.provider_type {
                ProviderType::Ollama => "http://localhost:11434",
                _ => "https://openrouter.ai/api/v1",
            });
        let url = format!("{}/v1/chat/completions", base.trim_end_matches('/'));

        let msgs: Vec<serde_json::Value> = messages
            .iter()
            .map(|m| {
                serde_json::json!({
                    "role": Self::role_str(&m.role),
                    "content": &m.content,
                })
            })
            .collect();

        let mut body = serde_json::json!({
            "model": model,
            "messages": msgs,
            "temperature": self.config.temperature,
        });

        if let Some(max) = self.config.max_tokens {
            body["max_tokens"] = serde_json::json!(max);
        }

        if let Some(tools_json) = tools {
            let normalized = Self::normalize_tools(tools_json);
            if !normalized.is_empty() {
                let openai_tools: Vec<serde_json::Value> = normalized
                    .iter()
                    .map(|t| serde_json::json!({"type": "function", "function": t}))
                    .collect();
                body["tools"] = serde_json::json!(openai_tools);
            }
        }

        let mut req = self.client.post(&url);
        if let Some(ref key) = self.config.api_key {
            req = req.header("Authorization", format!("Bearer {}", key));
        }
        req = req.header("Content-Type", "application/json");
        req.json(&body)
    }

    async fn complete_openai(
        &self,
        model: &str,
        messages: Vec<Message>,
        tools: Option<String>,
    ) -> Result<CompletionResponse, String> {
        let req = self.build_request(model, &messages, &tools);

        let resp = req
            .send()
            .await
            .map_err(|e| format!("HTTP request failed: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("LLM API returned {status}: {text}"));
        }

        let data: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("Failed to parse response: {e}"))?;

        Self::parse_openai_response(&data)
    }

    fn parse_openai_response(data: &serde_json::Value) -> Result<CompletionResponse, String> {
        let choice = data
            .pointer("/choices/0/message")
            .ok_or("No choices in response")?;

        let content = choice
            .get("content")
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .to_string();

        let tool_calls = choice
            .get("tool_calls")
            .and_then(|tc| tc.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|tc| {
                        let func = tc.get("function")?;
                        Some(ToolCall {
                            id: tc
                                .get("id")
                                .and_then(|i| i.as_str())
                                .unwrap_or("")
                                .to_string(),
                            name: func
                                .get("name")
                                .and_then(|n| n.as_str())
                                .unwrap_or("")
                                .to_string(),
                            arguments: func
                                .get("arguments")
                                .and_then(|a| a.as_str())
                                .unwrap_or("{}")
                                .to_string(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        let usage = data.get("usage").map(|u| TokenUsage {
            input_tokens: u.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
            output_tokens: u
                .get("completion_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32,
        });

        Ok(CompletionResponse {
            content,
            tool_calls,
            usage,
        })
    }
}

impl LlmProvider for HostLlmProvider {
    fn complete(
        &self,
        model: &str,
        messages: Vec<Message>,
        tools: Option<String>,
    ) -> Result<CompletionResponse, String> {
        let effective_model = self.effective_model(model);
        debug!(
            provider = ?self.config.provider_type,
            model = %effective_model,
            message_count = messages.len(),
            has_tools = tools.is_some(),
            "HostLlmProvider::complete"
        );

        // The LlmProvider trait is synchronous but we need async HTTP.
        // Use block_in_place + block_on as recommended in the trait docs.
        tokio::task::block_in_place(|| {
            let handle = tokio::runtime::Handle::current();
            handle.block_on(async {
                // All providers (OpenRouter, Ollama, AzureOpenAI) use OpenAI-compatible API
                self.complete_openai(&effective_model, messages, tools)
                    .await
            })
        })
    }
}

/// Build the LLM provider from the current GPUI globals.
///
/// Picks the first configured model and its provider to serve as the host
/// LLM for WASM modules. Returns `None` if no models/providers are configured.
fn build_llm_provider(cx: &App) -> Option<Arc<dyn LlmProvider>> {
    use crate::settings::models::{ModelsModel, ProviderModel};

    let models = cx.try_global::<ModelsModel>()?;
    let providers = cx.try_global::<ProviderModel>()?;

    let model_config = models.models().first()?;
    let provider_config = providers
        .providers()
        .iter()
        .find(|p| p.provider_type == model_config.provider_type)?;

    info!(
        provider = ?provider_config.provider_type,
        model = %model_config.model_identifier,
        "Building HostLlmProvider for WASM modules"
    );

    let config = LlmConfig {
        provider_type: provider_config.provider_type.clone(),
        api_key: provider_config.api_key.clone(),
        base_url: provider_config.base_url.clone(),
        model_identifier: model_config.model_identifier.clone(),
        temperature: model_config.temperature,
        max_tokens: model_config.max_tokens,
    };

    Some(Arc::new(HostLlmProvider::new(config)))
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
        match ModuleRegistry::new(noop_provider(), ResourceLimits::default()) {
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
                let execution_mode = manifest.execution_mode.clone();
                let is_remote = matches!(execution_mode.as_str(), "remote" | "remote_only");

                // Remote modules run on hive-runner — skip WASM loading entirely.
                let (status, wasm_file) = if is_remote {
                    (ModuleLoadStatus::Remote, "remote".to_string())
                } else {
                    let st = match validation_registry.load(&module_dir) {
                        Ok(_) => ModuleLoadStatus::Loaded,
                        Err(err) => ModuleLoadStatus::Error(err.to_string()),
                    };
                    let wf = manifest
                        .wasm_path
                        .as_ref()
                        .and_then(|p| p.file_name())
                        .and_then(|name| name.to_str())
                        .unwrap_or("unknown")
                        .to_string();
                    (st, wf)
                };

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
                    execution_mode,
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
                    execution_mode: "local".to_string(),
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

    // Notify the active agent to rebuild so it picks up newly
    // discovered (or removed) module agents.
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

fn apply_gateway_result(
    settings: &ModuleSettingsModel,
    generation: u64,
    result: Result<ProtocolGateway>,
    cx: &mut App,
) {
    let gateway_ok;
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
                gateway_ok = true;
            }
            Err(err) => {
                state.gateway_status = format!("Gateway failed to start: {err}");
                state.gateway = None;
                gateway_ok = false;
            }
        }
    }

    if gateway_ok {
        sync_module_mcp_servers(settings.gateway_port, cx);
    } else {
        remove_module_mcp_servers(cx);
    }

    cx.refresh_windows();
}

/// Add/update MCP server entries for discovered modules that declare `mcp = true`.
/// Entries are created disabled so the user must manually enable them.
fn sync_module_mcp_servers(gateway_port: u16, cx: &mut App) {
    let mcp_modules: Vec<String> = {
        let discovered = cx.global::<DiscoveredModulesModel>();
        discovered
            .modules
            .iter()
            .filter(|m| m.mcp && matches!(m.status, ModuleLoadStatus::Loaded))
            .map(|m| m.name.clone())
            .collect()
    };

    if mcp_modules.is_empty() {
        return;
    }

    let mut changed = false;
    {
        let model = cx.global_mut::<McpServersModel>();
        for module_name in &mcp_modules {
            let url = format!("http://127.0.0.1:{}/mcp/{}", gateway_port, module_name);

            if let Some(existing) = model
                .servers_mut()
                .iter_mut()
                .find(|s| s.name == *module_name && s.is_module)
            {
                // Update URL if gateway port changed
                if existing.url != url {
                    info!(module = %module_name, url = %url, "Updated module MCP server URL");
                    existing.url = url;
                    changed = true;
                }
            } else if !model.servers().iter().any(|s| s.name == *module_name) {
                // Only create if no server (manual or module) already has this name
                info!(module = %module_name, "Auto-registered module as MCP server (disabled)");
                model.servers_mut().push(McpServerConfig {
                    name: module_name.clone(),
                    url,
                    api_key: None,
                    enabled: false,
                    is_module: true,
                });
                changed = true;
            }
        }

        // Remove module entries for modules that are no longer discovered
        let before = model.servers().len();
        model
            .servers_mut()
            .retain(|s| !s.is_module || mcp_modules.contains(&s.name));
        if model.servers().len() != before {
            changed = true;
        }
    }

    if changed {
        let servers = cx.global::<McpServersModel>().servers().to_vec();
        save_mcp_servers_async(servers, cx);
    }
}

/// Remove all module-sourced MCP server entries (e.g. when gateway stops).
fn remove_module_mcp_servers(cx: &mut App) {
    let changed = {
        let model = cx.global_mut::<McpServersModel>();
        let before = model.servers().len();
        model.servers_mut().retain(|s| !s.is_module);
        model.servers().len() != before
    };

    if changed {
        let servers = cx.global::<McpServersModel>().servers().to_vec();
        save_mcp_servers_async(servers, cx);
    }
}

fn save_mcp_servers_async(servers: Vec<McpServerConfig>, cx: &mut App) {
    cx.spawn(|_cx: &mut AsyncApp| async move {
        let repo = chatty_core::mcp_repository();
        if let Err(e) = repo.save_all(servers).await {
            error!(error = ?e, "Failed to save MCP servers after module sync");
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
                    state.gateway_status = format!(
                        "Gateway will start on the first delegation (http://127.0.0.1:{})",
                        settings.gateway_port
                    );
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

            let gateway_result = match registry_result {
                Ok(registry) => {
                    let shared = Arc::new(tokio::sync::RwLock::new(registry));
                    let mut gateway = ProtocolGateway::new(shared, settings.gateway_port);

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

                        // Wire UsageCollector for post-invocation usage reporting
                        let usage_config = UsageCollectorConfig::default();
                        let usage_collector = Arc::new(UsageCollector::new(
                            &hive_settings.registry_url,
                            usage_config,
                        ));
                        if let Some(session) = session {
                            let uc = Arc::clone(&usage_collector);
                            tokio::spawn(async move { uc.set_session(session).await });
                        }
                        // Start the periodic flush task — without this, events
                        // accumulate in memory but are never sent to the registry,
                        // so dashboard analytics never increment.
                        usage_collector.start_background_flush();

                        gateway = gateway
                            .with_hive_client(hive_client)
                            .with_runner_url(hive_settings.runner_url)
                            .with_credit_guard(credit_guard)
                            .with_usage_collector(usage_collector)
                            .with_paid_modules(paid_modules);
                    }

                    // ADR-0011 C2: the gateway is also the fleet broker.
                    // Each virtual agent — `local-agent`, or the named team
                    // module settings declare (C10) — spawns one child per
                    // delegated task, on a connection the broker makes for
                    // it (ADR-0020); the shared socket beside the HTTP port
                    // refuses every registration. Unix only — the runners do
                    // not exist on Windows, so the gateway there is just the
                    // gateway.
                    #[cfg(unix)]
                    {
                        let participants = gateway.participants();
                        broker_runner::serve_socket(&broker_runner::socket_path());
                        let (workspace_dir, specs) = cx
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
                                // The roster's specs, looked up from the
                                // workspace a worker's tree comes from. A
                                // spec that does not load leaves the
                                // broker without workers, loudly.
                                let agents = match load_roster(
                                    &settings.virtual_agents,
                                    exec.workspace_dir.as_deref().map(Path::new),
                                ) {
                                    Ok(agents) => agents,
                                    Err(e) => {
                                        error!(error = ?e, "Failed to load the virtual agents' specs");
                                        return (exec.workspace_dir.clone(), Vec::new());
                                    }
                                };
                                let specs = resolve_virtual_agents(
                                    models,
                                    providers,
                                    &settings,
                                    &agents,
                                    &common_args,
                                );
                                (exec.workspace_dir.clone(), specs)
                            })
                            .unwrap_or((None, Vec::new()));
                        for runner in broker_runner::local_runners(
                            participants,
                            workspace_dir,
                            settings.default_endpoint_budget,
                            specs,
                        ) {
                            gateway = gateway.with_virtual_agent(Arc::new(runner));
                        }
                    }

                    gateway.start().await.map(|_| gateway)
                }
                Err(err) => Err(err),
            };

            let port = settings.gateway_port;
            let error_text = gateway_result.as_ref().err().map(|e| e.to_string());
            let _ = cx.update(|cx| {
                apply_gateway_result(&settings, generation, gateway_result, cx);
            });
            let _ = reply.send(match error_text {
                None => Ok(port),
                Some(error) => Err(error),
            });
        }
    })
    .detach();
}

// ---------------------------------------------------------------------------
// PL-E7 (AGE-602), evaluation-plan S4 rows 4.1-4.4.
//
// Rows marked `#[ignore = "known defect: PL-H2 (AGE-605)"]` or
// `#[ignore = "known defect: PL-H9 (AGE-612)"]` fail on today's code for the
// reason named in the evaluation plan's F3/F13 findings; `-- --ignored`
// lists them. Nothing here is fixed — tests only.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod host_llm_provider_tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn config(
        provider_type: ProviderType,
        api_key: Option<&str>,
        base_url: Option<&str>,
    ) -> LlmConfig {
        LlmConfig {
            provider_type,
            api_key: api_key.map(str::to_string),
            base_url: base_url.map(str::to_string),
            model_identifier: "test-model".to_string(),
            temperature: 0.0,
            max_tokens: None,
        }
    }

    fn one_message() -> Vec<Message> {
        vec![Message {
            role: Role::User,
            content: "hi".to_string(),
        }]
    }

    fn ok_body() -> serde_json::Value {
        serde_json::json!({
            "choices": [{"message": {"content": "hi", "role": "assistant"}}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1},
        })
    }

    // -- 4.1: URL + auth header per provider -------------------------------

    /// OpenRouter with no base URL override: the code hardcodes
    /// `https://openrouter.ai/api/v1` then appends `/v1/chat/completions`,
    /// doubling the `/v1` segment (F3). Inspecting the *built* (unsent)
    /// request proves the doubling without a real network call.
    #[test]
    #[ignore = "known defect: PL-H2 (AGE-605)"]
    fn row_4_1_openrouter_default_base_url_is_not_doubled() {
        let provider =
            HostLlmProvider::new(config(ProviderType::OpenRouter, Some("sk-test"), None));
        let req = provider
            .build_request("test-model", &one_message(), &None)
            .build()
            .expect("request should build");
        assert_eq!(
            req.url().as_str(),
            "https://openrouter.ai/api/v1/chat/completions",
            "the default OpenRouter base already ends in /v1; appending \
             /v1/chat/completions must not double it (today it builds {})",
            req.url()
        );
    }

    /// Ollama with no base URL override resolves to the documented default
    /// port and is not affected by F3 (no `/v1` in the hardcoded base).
    #[test]
    fn row_4_1_ollama_default_base_url() {
        let provider = HostLlmProvider::new(config(ProviderType::Ollama, None, None));
        let req = provider
            .build_request("test-model", &one_message(), &None)
            .build()
            .expect("request should build");
        assert_eq!(
            req.url().as_str(),
            "http://localhost:11434/v1/chat/completions"
        );
        assert!(
            req.headers().get("Authorization").is_none(),
            "no api key configured, so no bearer header should be sent"
        );
    }

    /// A custom base URL (no trailing `/v1`) round-trips correctly against a
    /// real (fake) OpenAI-compatible endpoint, with a bearer auth header.
    #[tokio::test(flavor = "multi_thread")]
    async fn row_4_1_custom_base_url_and_bearer_header() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(header("Authorization", "Bearer sk-custom"))
            .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
            .expect(1)
            .mount(&server)
            .await;

        let provider = HostLlmProvider::new(config(
            ProviderType::OpenRouter,
            Some("sk-custom"),
            Some(&server.uri()),
        ));
        let resp = provider
            .complete_openai("test-model", one_message(), None)
            .await
            .expect("wiremock should answer 200");
        assert_eq!(resp.content, "hi");
    }

    /// Azure OpenAI is not handled at all (F3): it should authenticate with
    /// an `api-key` header (and its own URL shape), but `HostLlmProvider`
    /// sends the same `Authorization: Bearer` header it sends everyone else.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "known defect: PL-H2 (AGE-605)"]
    async fn row_4_1_azure_uses_api_key_header_not_bearer() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(header("api-key", "sk-azure"))
            .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
            .expect(1)
            .mount(&server)
            .await;

        let provider = HostLlmProvider::new(config(
            ProviderType::AzureOpenAI,
            Some("sk-azure"),
            Some(&server.uri()),
        ));
        // Today: sends `Authorization: Bearer sk-azure` instead, so the
        // mock above (which requires `api-key`) never matches and this
        // errors out (wiremock reports the unmatched request on verify).
        provider
            .complete_openai("test-model", one_message(), None)
            .await
            .expect("Azure should authenticate with api-key, not bearer");
    }

    // -- 4.2: current-thread runtime -----------------------------------

    /// `block_in_place` panics unconditionally on a `current_thread`
    /// runtime (tokio's own contract). `HostLlmProvider::complete` is the
    /// synchronous `LlmProvider` entry point a WASM guest's `llm::complete`
    /// import calls into; today it always panics there instead of erroring
    /// or being unreachable on that runtime flavor.
    #[test]
    #[ignore = "known defect: PL-H2 (AGE-605)"]
    fn row_4_2_current_thread_runtime_does_not_panic() {
        let provider = HostLlmProvider::new(config(ProviderType::Ollama, None, None));
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("current-thread runtime should build");
        let result = rt.block_on(async {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                LlmProvider::complete(&provider, "test-model", one_message(), None)
            }))
        });
        assert!(
            result.is_ok(),
            "HostLlmProvider::complete panicked on a current-thread runtime \
             (block_in_place always panics there)"
        );
    }
}

#[cfg(test)]
mod module_mcp_sync_tests {
    use super::*;
    use gpui::TestAppContext;

    /// 4.3: `sync_module_mcp_servers` adds a disabled entry per `mcp = true`
    /// module, leaves a manually-added server with the same name alone, and
    /// removes module entries for modules that disappeared — without
    /// touching non-module entries.
    #[gpui::test]
    async fn row_4_3_add_rename_remove_and_manual_clash(cx: &mut TestAppContext) {
        // `sync_module_mcp_servers` ends by detaching a `cx.spawn` task that
        // persists the servers through `chatty_core::mcp_repository()`
        // (tokio::fs under the hood); gpui's own test executor is not a
        // Tokio runtime, so that detached task needs one to schedule its
        // blocking I/O on. Entering one here is test setup, not a change to
        // `sync_module_mcp_servers` itself, whose synchronous global-state
        // update (what this row checks) already happened by the time it
        // spawns that task.
        // `sync_module_mcp_servers` ends by detaching a `cx.spawn` task that
        // persists the servers through `chatty_core::mcp_repository()`
        // (tokio::fs under the hood). `gpui::test`'s own teardown drives any
        // still-pending foreground task on this same OS thread *after* this
        // function returns (so a plain `Runtime::enter()` guard, dropped at
        // the end of this function, is gone before that happens) — this
        // thread needs a Tokio context installed for the rest of its life,
        // not just for the body below. Leaking the guard is test setup for
        // that one thread, not a change to `sync_module_mcp_servers`, whose
        // synchronous global-state update (what this row checks) is already
        // done by the time it spawns that task.
        let rt: &'static tokio::runtime::Runtime = Box::leak(Box::new(
            tokio::runtime::Runtime::new().expect("tokio runtime"),
        ));
        std::mem::forget(rt.enter());

        cx.update(|cx| {
            let _ = chatty_core::init_repositories();
            cx.set_global(DiscoveredModulesModel {
                modules: vec![
                    module_entry("echo-agent", true),
                    module_entry("benford-agent", true),
                ],
                ..Default::default()
            });
            cx.set_global(McpServersModel::new());

            // A manually-added server sharing a module's name: must be left
            // untouched (not overwritten into a module entry).
            cx.global_mut::<McpServersModel>()
                .servers_mut()
                .push(McpServerConfig {
                    name: "benford-agent".to_string(),
                    url: "http://example.invalid/mcp".to_string(),
                    api_key: Some("manual-key".to_string()),
                    enabled: true,
                    is_module: false,
                });

            sync_module_mcp_servers(9420, cx);

            let servers = cx.global::<McpServersModel>().servers();
            let echo = servers
                .iter()
                .find(|s| s.name == "echo-agent")
                .expect("echo-agent should be auto-registered");
            assert!(echo.is_module);
            assert!(!echo.enabled, "auto-registered entries start disabled");
            assert_eq!(echo.url, "http://127.0.0.1:9420/mcp/echo-agent");

            let manual = servers
                .iter()
                .find(|s| s.name == "benford-agent" && !s.is_module)
                .expect("the manual server must survive the clash untouched");
            assert_eq!(manual.url, "http://example.invalid/mcp");
            assert_eq!(manual.api_key.as_deref(), Some("manual-key"));
            assert!(
                !servers
                    .iter()
                    .any(|s| s.name == "benford-agent" && s.is_module),
                "a module must not create a second entry under a manually \
                 used name"
            );

            // Now the module is renamed/removed (only echo-agent remains
            // discovered): the stale module entry for benford-agent's
            // module-side registration (none was created above) stays
            // gone, and a rename shows up as add-new + remove-old.
            cx.global_mut::<DiscoveredModulesModel>().modules =
                vec![module_entry("echo-agent-renamed", true)];
            sync_module_mcp_servers(9420, cx);

            let servers = cx.global::<McpServersModel>().servers();
            assert!(
                servers
                    .iter()
                    .any(|s| s.name == "echo-agent-renamed" && s.is_module),
                "the renamed module should be registered under its new name"
            );
            assert!(
                !servers
                    .iter()
                    .any(|s| s.name == "echo-agent" && s.is_module),
                "the old module name's entry should be removed on rename"
            );
            assert!(
                servers
                    .iter()
                    .any(|s| s.name == "benford-agent" && !s.is_module),
                "the manual server is still there, untouched by the rename"
            );
        });
    }

    fn module_entry(name: &str, mcp: bool) -> DiscoveredModuleEntry {
        DiscoveredModuleEntry {
            directory_name: name.to_string(),
            name: name.to_string(),
            version: "0.1.0".to_string(),
            description: String::new(),
            wasm_file: format!("{name}.wasm"),
            tools: Vec::new(),
            chat: true,
            agent: true,
            openai_compat: true,
            mcp,
            a2a: true,
            status: ModuleLoadStatus::Loaded,
            execution_mode: "local".to_string(),
        }
    }
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
            chat: true,
            agent: true,
            openai_compat: true,
            mcp,
            a2a: true,
            status: ModuleLoadStatus::Loaded,
            execution_mode: "local".to_string(),
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
