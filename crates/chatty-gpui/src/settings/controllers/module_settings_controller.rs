use crate::settings::models::mcp_store::{McpServerConfig, McpServersModel};
use crate::settings::models::module_settings::{
    ModuleSettingsModel, default_module_dir, normalize_module_dir,
};
use crate::settings::models::{
    AgentConfigEvent, DiscoveredModuleEntry, DiscoveredModulesModel, GlobalAgentConfigNotifier,
    ModuleLoadStatus,
};
use anyhow::{Context, Result};
use chatty_core::settings::models::providers_store::ProviderType;
use chatty_module_registry::{ModuleManifest, ModuleRegistry};
use chatty_protocol_gateway::ProtocolGateway;
use chatty_wasm_runtime::{
    CompletionResponse, LlmProvider, Message, ResourceLimits, Role, TokenUsage, ToolCall,
};
use gpui::{App, AsyncApp};
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
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .unwrap_or_default();
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

    async fn complete_openai(
        &self,
        model: &str,
        messages: Vec<Message>,
        tools: Option<String>,
    ) -> Result<CompletionResponse, String> {
        let base = self
            .config
            .base_url
            .as_deref()
            .unwrap_or(match self.config.provider_type {
                ProviderType::Ollama => "http://localhost:11434",
                ProviderType::Mistral => "https://api.mistral.ai",
                _ => "https://api.openai.com",
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

        if let Some(ref tools_json) = tools
            && let Ok(parsed) = serde_json::from_str::<serde_json::Value>(tools_json)
        {
            body["tools"] = parsed;
        }

        let mut req = self.client.post(&url);
        if let Some(ref key) = self.config.api_key {
            req = req.header("Authorization", format!("Bearer {}", key));
        }
        req = req.header("Content-Type", "application/json");

        let resp = req
            .json(&body)
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

    async fn complete_anthropic(
        &self,
        model: &str,
        messages: Vec<Message>,
        tools: Option<String>,
    ) -> Result<CompletionResponse, String> {
        let base = self
            .config
            .base_url
            .as_deref()
            .unwrap_or("https://api.anthropic.com");
        let url = format!("{}/v1/messages", base.trim_end_matches('/'));

        let (system_msg, chat_msgs): (Vec<_>, Vec<_>) = messages
            .iter()
            .partition(|m| matches!(m.role, Role::System));

        let msgs: Vec<serde_json::Value> = chat_msgs
            .iter()
            .map(|m| {
                serde_json::json!({
                    "role": Self::role_str(&m.role),
                    "content": &m.content,
                })
            })
            .collect();

        let max_tokens = self.config.max_tokens.unwrap_or(4096);

        let mut body = serde_json::json!({
            "model": model,
            "messages": msgs,
            "max_tokens": max_tokens,
            "temperature": self.config.temperature,
        });

        if let Some(sys) = system_msg.first() {
            body["system"] = serde_json::json!(&sys.content);
        }

        if let Some(ref tools_json) = tools
            && let Ok(parsed) = serde_json::from_str::<Vec<serde_json::Value>>(tools_json)
        {
            let anthropic_tools: Vec<serde_json::Value> = parsed
                    .iter()
                    .filter_map(|t| {
                        let func = t.get("function")?;
                        Some(serde_json::json!({
                            "name": func.get("name")?,
                            "description": func.get("description").unwrap_or(&serde_json::json!("")),
                            "input_schema": func.get("parameters").unwrap_or(&serde_json::json!({"type": "object"})),
                        }))
                    })
                    .collect();
            if !anthropic_tools.is_empty() {
                body["tools"] = serde_json::json!(anthropic_tools);
            }
        }

        let api_key = self
            .config
            .api_key
            .as_deref()
            .ok_or("Anthropic API key not configured")?;

        let resp = self
            .client
            .post(&url)
            .header("x-api-key", api_key)
            .header("anthropic-version", "2023-06-01")
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("HTTP request failed: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("Anthropic API returned {status}: {text}"));
        }

        let data: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("Failed to parse response: {e}"))?;

        Self::parse_anthropic_response(&data)
    }

    async fn complete_gemini(
        &self,
        model: &str,
        messages: Vec<Message>,
        tools: Option<String>,
    ) -> Result<CompletionResponse, String> {
        let base = self
            .config
            .base_url
            .as_deref()
            .unwrap_or("https://generativelanguage.googleapis.com");
        let api_key = self
            .config
            .api_key
            .as_deref()
            .ok_or("Gemini API key not configured")?;
        let url = format!(
            "{}/v1beta/models/{}:generateContent",
            base.trim_end_matches('/'),
            model,
        );

        let contents: Vec<serde_json::Value> = messages
            .iter()
            .filter(|m| !matches!(m.role, Role::System))
            .map(|m| {
                let role = match m.role {
                    Role::User => "user",
                    _ => "model",
                };
                serde_json::json!({
                    "role": role,
                    "parts": [{"text": &m.content}],
                })
            })
            .collect();

        let mut body = serde_json::json!({ "contents": contents });

        if let Some(sys) = messages.iter().find(|m| matches!(m.role, Role::System)) {
            body["systemInstruction"] = serde_json::json!({"parts": [{"text": &sys.content}]});
        }

        body["generationConfig"] = serde_json::json!({
            "temperature": self.config.temperature,
        });
        if let Some(max) = self.config.max_tokens {
            body["generationConfig"]["maxOutputTokens"] = serde_json::json!(max);
        }

        // Tools mapping for Gemini (simplified — function declarations only)
        if let Some(ref tools_json) = tools
            && let Ok(parsed) = serde_json::from_str::<Vec<serde_json::Value>>(tools_json)
        {
            let decls: Vec<serde_json::Value> = parsed
                    .iter()
                    .filter_map(|t| {
                        let func = t.get("function")?;
                        Some(serde_json::json!({
                            "name": func.get("name")?,
                            "description": func.get("description").unwrap_or(&serde_json::json!("")),
                            "parameters": func.get("parameters").unwrap_or(&serde_json::json!({"type": "object"})),
                        }))
                    })
                    .collect();
            if !decls.is_empty() {
                body["tools"] = serde_json::json!([{"functionDeclarations": decls}]);
            }
        }

        let resp = self
            .client
            .post(&url)
            .header("x-goog-api-key", api_key)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("HTTP request failed: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("Gemini API returned {status}: {text}"));
        }

        let data: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("Failed to parse response: {e}"))?;

        // Parse Gemini response
        let candidate = data
            .pointer("/candidates/0/content/parts")
            .and_then(|p| p.as_array());

        let mut content = String::new();
        let mut tool_calls = Vec::new();

        if let Some(parts) = candidate {
            for part in parts {
                if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                    content.push_str(text);
                }
                if let Some(fc) = part.get("functionCall") {
                    tool_calls.push(ToolCall {
                        id: uuid::Uuid::new_v4().to_string(),
                        name: fc
                            .get("name")
                            .and_then(|n| n.as_str())
                            .unwrap_or("")
                            .to_string(),
                        arguments: fc
                            .get("args")
                            .map(|a| a.to_string())
                            .unwrap_or_else(|| "{}".to_string()),
                    });
                }
            }
        }

        let usage = data.get("usageMetadata").map(|u| TokenUsage {
            input_tokens: u
                .get("promptTokenCount")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32,
            output_tokens: u
                .get("candidatesTokenCount")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32,
        });

        Ok(CompletionResponse {
            content,
            tool_calls,
            usage,
        })
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

    fn parse_anthropic_response(data: &serde_json::Value) -> Result<CompletionResponse, String> {
        let blocks = data
            .get("content")
            .and_then(|c| c.as_array())
            .ok_or("No content in Anthropic response")?;

        let mut content = String::new();
        let mut tool_calls = Vec::new();

        for block in blocks {
            match block.get("type").and_then(|t| t.as_str()) {
                Some("text") => {
                    if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                        content.push_str(text);
                    }
                }
                Some("tool_use") => {
                    tool_calls.push(ToolCall {
                        id: block
                            .get("id")
                            .and_then(|i| i.as_str())
                            .unwrap_or("")
                            .to_string(),
                        name: block
                            .get("name")
                            .and_then(|n| n.as_str())
                            .unwrap_or("")
                            .to_string(),
                        arguments: block
                            .get("input")
                            .map(|i| i.to_string())
                            .unwrap_or_else(|| "{}".to_string()),
                    });
                }
                _ => {}
            }
        }

        let usage = data.get("usage").map(|u| TokenUsage {
            input_tokens: u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
            output_tokens: u.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
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
                match self.config.provider_type {
                    ProviderType::Anthropic => {
                        self.complete_anthropic(&effective_model, messages, tools)
                            .await
                    }
                    ProviderType::Gemini => {
                        self.complete_gemini(&effective_model, messages, tools)
                            .await
                    }
                    // OpenAI, Ollama, Mistral, AzureOpenAI all use OpenAI-compatible API
                    _ => {
                        self.complete_openai(&effective_model, messages, tools)
                            .await
                    }
                }
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

    // Notify the active agent to rebuild so it picks up newly
    // discovered (or removed) module agents.
    if let Some(weak_notifier) = cx
        .try_global::<GlobalAgentConfigNotifier>()
        .and_then(|g| g.entity.clone())
        && let Some(notifier) = weak_notifier.upgrade()
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
                let provider = llm_provider.clone();
                move || build_registry(&module_dir, provider)
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
