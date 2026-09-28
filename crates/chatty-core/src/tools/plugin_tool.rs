//! A spec's WASM plugins as tools of the agent that lists them (PL-U2,
//! AGE-616).
//!
//! An [`AgentSpec`](crate::agent_spec::AgentSpec)'s `[[plugins]]` are loaded
//! when the agent is built: one [`WasmModule`] instance per (agent, plugin),
//! resolved by module name from the host's module directories
//! ([`PluginHost::module_roots`]) and checked against the spec's `version`.
//! Each tool the module's `list-tools` names becomes a rig tool of the agent
//! ([`PluginTool`]), called in-process — no MCP client, no HTTP, no gateway
//! lock — under the per-call limits of PL-H1 (the module's `[resources]`,
//! lowered by the spec's `limits`).
//!
//! * **Names.** A plugin tool is advertised as `<plugin>__<tool>`
//!   ([`plugin_tool_name`]). `<plugin>.<tool>` reads better but is not a
//!   legal tool name on OpenAI-wire providers (OpenRouter, Azure OpenAI:
//!   `^[a-zA-Z0-9_-]{1,64}$`); `__` is legal everywhere, and a native tool
//!   never contains it, so two plugins cannot collide with each other or
//!   with a native tool. A module whose name or tool name would make an
//!   illegal name fails to load, naming the rule. The UI shows the call as
//!   `<plugin>.<tool>` ([`plugin_tool_display_name`]).
//! * **Errors** go through [`map_tool_error`](super::map_tool_error), so a
//!   trap, a deadline or the guest's own error reaches the model with its
//!   reason and the turn goes on.
//! * **Approvals** are a function of the spec's grants, not of the tool name
//!   ([`plugin_needs_approval`]): a plugin granted nothing side-effecting
//!   runs without asking.
//! * **Usage.** A plugin's `llm::complete` goes through
//!   [`PluginLlmProvider`] on the calling agent's model; the calls it served
//!   are drained into the turn by `stream_prompt` as their own usage line,
//!   attributed to the plugin ([`TokenUsage::plugin`](crate::models::token_usage::TokenUsage::plugin)).

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use anyhow::{Context, Result, bail};
use chatty_module_registry::ModuleManifest;
use chatty_wasm_runtime::{Engine, LlmProvider, ResourceLimits, ToolCallRequest, WasmModule};
use rig_agent::tool::{DynamicTool, ToolOutput};
use tokio::sync::Mutex;

use crate::agent_spec::PluginSpec;
use crate::models::execution_approval_store::{PendingApprovals, request_execution_approval};
use crate::services::plugin_llm::{PluginLlmProvider, PluginUsage};
use crate::settings::models::execution_settings::ApprovalMode;
use crate::settings::models::models_store::ModelConfig;
use crate::settings::models::providers_store::ProviderConfig;

use super::{ToolError, map_tool_error};

/// Between a plugin's name and its tool's in the advertised tool name.
pub const PLUGIN_TOOL_SEPARATOR: &str = "__";

/// The longest tool name OpenAI-wire providers accept.
pub const MAX_TOOL_NAME_LEN: usize = 64;

/// Capabilities whose use changes something outside the plugin, so a call
/// needs the user's approval (PL-U4 §4). `llm` costs money but counts
/// against the budget instead; `config` and `logging` are read-only.
pub const SIDE_EFFECTING_GRANTS: &[&str] = &["http", "file-write"];

/// `<plugin>__<tool>`: the name a plugin's tool is advertised under.
pub fn plugin_tool_name(plugin: &str, tool: &str) -> String {
    format!("{plugin}{PLUGIN_TOOL_SEPARATOR}{tool}")
}

/// `<plugin>.<tool>` for an advertised plugin tool name, as the transcripts
/// show it; `None` for any other tool.
pub fn plugin_tool_display_name(name: &str) -> Option<String> {
    let (plugin, tool) = name.split_once(PLUGIN_TOOL_SEPARATOR)?;
    (!plugin.is_empty() && !tool.is_empty()).then(|| format!("{plugin}.{tool}"))
}

/// Whether `name` is a legal tool name on every provider chatty talks to:
/// OpenAI-wire providers (OpenRouter, Azure OpenAI) require
/// `^[a-zA-Z0-9_-]{1,64}$`; Ollama accepts that and more.
pub fn is_provider_tool_name(name: &str) -> bool {
    (1..=MAX_TOOL_NAME_LEN).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Whether a plugin granted `grants` must ask before each call: only when a
/// grant is side-effecting ([`SIDE_EFFECTING_GRANTS`]).
pub fn plugin_needs_approval(grants: &[String]) -> bool {
    grants
        .iter()
        .any(|grant| SIDE_EFFECTING_GRANTS.contains(&grant.as_str()))
}

/// What a host gives the factory to load a spec's plugins with.
#[derive(Clone, Debug, Default)]
pub struct PluginHost {
    /// Directories searched in order for a module directory whose
    /// `module.toml` names the plugin; the first match wins. Usually just
    /// `ModuleSettingsModel::module_dir`.
    pub module_roots: Vec<PathBuf>,
    /// The user's configured models and their providers: the ones a
    /// plugin's `llm::complete` may name. An empty `model` is the calling
    /// agent's.
    pub models: Vec<ModelConfig>,
    pub providers: Vec<ProviderConfig>,
}

/// One tool a plugin offers, as the agent advertises it.
#[derive(Clone, Debug, PartialEq)]
pub struct PluginToolDef {
    /// `<plugin>__<tool>`.
    pub name: String,
    /// The tool's own name, what `invoke-tool` is called with.
    pub tool: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// One plugin of one agent: its instance, its tools and its usage log.
pub struct LoadedPlugin {
    /// The module name, as the spec names it.
    pub name: String,
    /// `[module].description`, for the `load_tools` catalog.
    pub description: String,
    pub tools: Vec<PluginToolDef>,
    /// The `llm::complete` calls this instance made, not yet drained.
    pub usage: PluginUsage,
    grants: Vec<String>,
    module: Arc<Mutex<WasmModule>>,
}

/// The one engine every plugin instance in this process is compiled on.
/// Limits are per store, not per engine, so one engine serves all.
fn engine() -> Result<Engine> {
    static PLUGIN_ENGINE: OnceLock<Engine> = OnceLock::new();
    if let Some(engine) = PLUGIN_ENGINE.get() {
        return Ok(engine.clone());
    }
    let engine = WasmModule::build_engine(&ResourceLimits::default())?;
    Ok(PLUGIN_ENGINE.get_or_init(|| engine).clone())
}

/// The module directory under `roots` whose `module.toml` names `module`,
/// with its manifest. Directories are visited in name order per root; a
/// manifest that does not parse is skipped (the registry reports it).
fn resolve_module(module: &str, roots: &[PathBuf]) -> Result<(PathBuf, ModuleManifest)> {
    for root in roots {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        let mut dirs: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|dir| dir.join("module.toml").is_file())
            .collect();
        dirs.sort();
        for dir in dirs {
            if let Ok(manifest) = ModuleManifest::from_file(&dir.join("module.toml"))
                && manifest.name == module
            {
                return Ok((dir, manifest));
            }
        }
    }
    bail!(
        "plugin `{module}`: no module named `{module}` in {}",
        if roots.is_empty() {
            "any module directory (none is configured)".to_string()
        } else {
            roots
                .iter()
                .map(|r| r.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        }
    )
}

/// The per-call limits: the host defaults, lowered by the module's
/// `[resources]`, lowered again by the spec's `limits`, then clamped to the
/// ceilings. Each layer may only lower a limit.
fn limits_for(manifest: &ModuleManifest, spec: &PluginSpec) -> ResourceLimits {
    let mut limits = ResourceLimits::default();
    let lower_memory_mb = |limits: &mut ResourceLimits, mb: u64| {
        if mb > 0 {
            limits.max_memory_bytes = limits.max_memory_bytes.min(mb.saturating_mul(1024 * 1024));
        }
    };
    let lower_ms = |limits: &mut ResourceLimits, ms: u64| {
        if ms > 0 {
            limits.max_execution_ms = limits.max_execution_ms.min(ms);
        }
    };
    lower_memory_mb(&mut limits, manifest.resources.max_memory_mb);
    lower_ms(&mut limits, manifest.resources.max_execution_ms);
    if let Some(mb) = spec.limits.max_memory_mb {
        lower_memory_mb(&mut limits, mb);
    }
    if let Some(ms) = spec.limits.max_execution_ms {
        lower_ms(&mut limits, ms);
    }
    limits.clamped()
}

/// Load one plugin for an agent: resolve its module, check the version,
/// instantiate it with its own [`PluginLlmProvider`] on `calling_model`,
/// and read its tools. Blocking (it compiles the component): call it off
/// the async executor.
pub fn load_plugin(
    spec: &PluginSpec,
    host: &PluginHost,
    calling_model: &ModelConfig,
    runtime: tokio::runtime::Handle,
) -> Result<LoadedPlugin> {
    let (dir, manifest) = resolve_module(&spec.module, &host.module_roots)?;
    if let Some(requirement) = spec.version.as_deref() {
        let requirement = semver::VersionReq::parse(requirement).with_context(|| {
            format!(
                "plugin `{}`: version `{requirement}` is not a semver requirement",
                spec.module
            )
        })?;
        let version = semver::Version::parse(&manifest.version).with_context(|| {
            format!(
                "plugin `{}`: module version `{}` is not semver",
                spec.module, manifest.version
            )
        })?;
        if !requirement.matches(&version) {
            bail!(
                "plugin `{}`: {} has version {version}, the spec asks for {requirement}",
                spec.module,
                dir.display()
            );
        }
    }
    if manifest.execution_mode.is_remote() {
        bail!(
            "plugin `{}`: the module runs remotely ({}), not in-process",
            spec.module,
            manifest.execution_mode
        );
    }
    let wasm_path = manifest
        .wasm_path
        .as_deref()
        .with_context(|| format!("plugin `{}`: the manifest names no wasm", spec.module))?;

    // What the guest reads: the manifest's `[config]` with the spec's on
    // top, and the manifest's `[files].root` (PL-H3; nothing wider).
    let mut config = manifest.config.clone();
    config.extend(spec.config.clone());
    let mut runtime_manifest = config.iter().fold(
        chatty_wasm_runtime::ModuleManifest::new(&manifest.name),
        |m, (key, value)| m.with_config(key, value),
    );
    if let Some(root) = &manifest.files_root {
        runtime_manifest = runtime_manifest.with_weights_root(root);
    }

    let provider = PluginLlmProvider::new(
        calling_model.clone(),
        host.models.clone(),
        host.providers.clone(),
        runtime,
    );
    let usage = provider.usage();
    let provider: Arc<dyn LlmProvider> = Arc::new(provider);
    // The install record's hash is checked on exactly the bytes compiled
    // (PL-H5a), as the module registry does at its loads.
    let load_context = || format!("plugin `{}`: failed to load {}", spec.module, dir.display());
    let bytes = std::fs::read(wasm_path).with_context(load_context)?;
    chatty_module_registry::install_record::verify_installed(&dir, &bytes)
        .with_context(load_context)?;
    let mut module = WasmModule::from_bytes(
        &engine()?,
        &bytes,
        runtime_manifest,
        provider,
        limits_for(&manifest, spec),
    )
    .with_context(load_context)?;
    let tools = module
        .list_tools()
        .with_context(|| format!("plugin `{}`: list-tools failed", spec.module))?
        .into_iter()
        .map(|tool| tool_def(&manifest.name, tool))
        .collect::<Result<Vec<_>>>()?;

    Ok(LoadedPlugin {
        name: manifest.name,
        description: manifest.description,
        tools,
        usage,
        grants: spec.grants.clone(),
        module: Arc::new(Mutex::new(module)),
    })
}

/// The advertised definition of one of `plugin`'s tools.
fn tool_def(plugin: &str, tool: chatty_wasm_runtime::ToolDefinition) -> Result<PluginToolDef> {
    let name = plugin_tool_name(plugin, &tool.name);
    if !is_provider_tool_name(&name) {
        bail!(
            "plugin `{plugin}`: tool `{}` would be advertised as `{name}`, which is not a legal \
             tool name (letters, digits, `_` and `-`, at most {MAX_TOOL_NAME_LEN} characters)",
            tool.name
        );
    }
    let parameters = if tool.parameters_schema.trim().is_empty() {
        serde_json::json!({ "type": "object", "properties": {} })
    } else {
        serde_json::from_str(&tool.parameters_schema).with_context(|| {
            format!(
                "plugin `{plugin}`: tool `{}` has a parameters schema that is not JSON",
                tool.name
            )
        })?
    };
    Ok(PluginToolDef {
        name,
        tool: tool.name,
        description: tool.description,
        parameters,
    })
}

/// Load every plugin `specs` lists, sorted by name so the tool block is the
/// same on every build (AGE-206). Any plugin that does not load fails the
/// whole build: a spec that lists a plugin depends on it.
pub async fn load_plugins(
    specs: &[PluginSpec],
    host: &PluginHost,
    calling_model: &ModelConfig,
) -> Result<Vec<LoadedPlugin>> {
    if specs.is_empty() {
        return Ok(Vec::new());
    }
    let runtime = tokio::runtime::Handle::current();
    let (specs, host, calling_model) = (specs.to_vec(), host.clone(), calling_model.clone());
    let mut plugins = tokio::task::spawn_blocking(move || {
        specs
            .iter()
            .map(|spec| load_plugin(spec, &host, &calling_model, runtime.clone()))
            .collect::<Result<Vec<_>>>()
    })
    .await
    .context("plugin loading panicked")??;
    plugins.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(plugins)
}

/// How a plugin tool that needs approval asks for it.
#[derive(Clone)]
pub struct PluginApprovals {
    pub pending: Option<PendingApprovals>,
    pub mode: ApprovalMode,
}

/// One tool of one loaded plugin, as a rig tool of the agent.
#[derive(Clone)]
pub struct PluginTool {
    def: PluginToolDef,
    module: Arc<Mutex<WasmModule>>,
    /// `Some` when the plugin's grants make every call ask first.
    approvals: Option<PluginApprovals>,
}

impl PluginTool {
    /// Every tool of `plugin`. `approvals` is consulted only when the
    /// plugin's grants need it ([`plugin_needs_approval`]).
    pub fn all(plugin: &LoadedPlugin, approvals: &PluginApprovals) -> Vec<Self> {
        let approvals = plugin_needs_approval(&plugin.grants).then(|| approvals.clone());
        plugin
            .tools
            .iter()
            .map(|def| Self {
                def: def.clone(),
                module: Arc::clone(&plugin.module),
                approvals: approvals.clone(),
            })
            .collect()
    }

    pub fn definition(&self) -> &PluginToolDef {
        &self.def
    }

    /// Run the tool: approval first when the grants ask for it, then
    /// `invoke-tool` under the module's own lock and per-call limits.
    pub async fn call(&self, args: serde_json::Value) -> Result<String, ToolError> {
        let args = match args {
            serde_json::Value::Null => "{}".to_string(),
            other => other.to_string(),
        };
        if let Some(approvals) = &self.approvals {
            let display =
                plugin_tool_display_name(&self.def.name).unwrap_or_else(|| self.def.name.clone());
            let Some(pending) = &approvals.pending else {
                return Err(ToolError::OperationFailed(
                    "this plugin was granted a side-effecting capability, so each call needs \
                     approval, and this agent has no one to ask"
                        .to_string(),
                ));
            };
            let approved = request_execution_approval(
                pending,
                &approvals.mode,
                &format!("[plugin] {display} {args}"),
                false,
            )
            .await?;
            if !approved {
                return Err(ToolError::OperationFailed(
                    "the user denied this call".to_string(),
                ));
            }
        }
        let call = ToolCallRequest {
            name: self.def.tool.clone(),
            arguments_json: args,
            // rig hands a dynamic tool no call id; the guest gets a fresh one.
            call_id: uuid::Uuid::new_v4().to_string(),
            caller: None,
        };
        let mut module = self.module.lock().await;
        // `ToolResult.usage` is the guest's own account of what it spent
        // through `llm::complete`; the host already counted each of those
        // calls in the plugin's `PluginUsage`, so it is not added again.
        module
            .invoke_tool(call)
            .await
            .map(|result| result.content)
            .map_err(|e| ToolError::OperationFailed(format!("{e:#}")))
    }

    /// The rig tool: the plugin's name/schema, errors through
    /// [`map_tool_error`] under the advertised name.
    pub fn into_dynamic(self) -> DynamicTool {
        let PluginToolDef {
            name,
            description,
            parameters,
            ..
        } = self.def.clone();
        let tool = Arc::new(self);
        DynamicTool::new(name, description, parameters, move |_context, args| {
            let tool = Arc::clone(&tool);
            Box::pin(async move {
                tool.call(args)
                    .await
                    .map(ToolOutput::text)
                    .map_err(|e| map_tool_error(&tool.def.name, e))
            })
        })
    }
}

/// The names the tools of `plugins` are advertised under, in order.
pub fn plugin_tool_names(plugins: &[LoadedPlugin]) -> impl Iterator<Item = &str> {
    plugins
        .iter()
        .flat_map(|plugin| plugin.tools.iter().map(|tool| tool.name.as_str()))
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use chatty_wasm_runtime::test_support::fixture_path;
    use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

    use super::*;
    use crate::agent_spec::{AgentSpec, PluginLimits};
    use crate::factories::agent_factory::{AgentBuildContext, AgentServices};
    use crate::models::token_usage::ModelRef;
    use crate::services::StreamSurface;
    use crate::session::{AgentSession, AgentSessionConfig, SessionEvent, TurnInput};
    use crate::settings::models::execution_settings::ExecutionSettingsModel;
    use crate::settings::models::providers_store::ProviderType;
    use crate::testing::fake_model::{FakeDaemon, RecordedRequest, Reply, Script, sse_stream};

    /// `target/wasm-fixtures`: one directory per staged module, each with
    /// its own `module.toml` — a module directory as a host has one.
    fn fixtures_root() -> PathBuf {
        fixture_path("echo")
            .parent()
            .and_then(|dir| dir.parent())
            .expect("fixtures live in target/wasm-fixtures/<name>/")
            .to_path_buf()
    }

    fn host() -> PluginHost {
        PluginHost {
            module_roots: vec![fixtures_root()],
            ..PluginHost::default()
        }
    }

    fn plugin(module: &str) -> PluginSpec {
        PluginSpec {
            module: module.to_string(),
            ..PluginSpec::default()
        }
    }

    fn model(provider: ProviderType, id: &str) -> ModelConfig {
        ModelConfig::new(id.to_string(), id.to_string(), provider, id.to_string())
    }

    async fn load_one(spec: PluginSpec) -> Result<LoadedPlugin> {
        load_plugins(&[spec], &host(), &model(ProviderType::Ollama, "m"))
            .await
            .map(|mut plugins| plugins.remove(0))
    }

    fn tool_of(plugin: &LoadedPlugin, name: &str) -> PluginTool {
        PluginTool::all(
            plugin,
            &PluginApprovals {
                pending: None,
                mode: ApprovalMode::AlwaysAsk,
            },
        )
        .into_iter()
        .find(|tool| tool.definition().name == name)
        .unwrap_or_else(|| panic!("{name} is registered"))
    }

    /// PL-H5a: a plugin whose `.wasm` no longer matches its install
    /// record is refused with the hash error, as at a registry load.
    #[tokio::test(flavor = "multi_thread")]
    async fn plugin_with_a_tampered_install_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("echo");
        std::fs::create_dir_all(&dir).unwrap();
        let staged = fixture_path("echo").parent().unwrap().to_path_buf();
        for file in ["echo.wasm", "module.toml"] {
            std::fs::copy(staged.join(file), dir.join(file)).unwrap();
        }
        chatty_module_registry::InstallRecord::new(
            b"other bytes",
            chatty_module_registry::TrustLevel::Signed,
            None,
        )
        .write(&dir)
        .unwrap();
        let host = PluginHost {
            module_roots: vec![root.path().to_path_buf()],
            ..PluginHost::default()
        };
        let err = load_plugins(&[plugin("echo")], &host, &model(ProviderType::Ollama, "m"))
            .await
            .err()
            .expect("a tampered plugin must not load");
        assert!(format!("{err:#}").contains("hash mismatch"), "{err:#}");
    }

    // -- names, approvals, limits --------------------------------------------

    /// `<plugin>__<tool>` passes the strictest provider rule (OpenAI-wire:
    /// `^[a-zA-Z0-9_-]{1,64}$`); `<plugin>.<tool>` does not, which is why it
    /// is only the display name.
    #[test]
    fn plugin_tool_names_are_legal_on_every_provider() {
        let name = plugin_tool_name("echo", "reverse");
        assert_eq!(name, "echo__reverse");
        assert!(is_provider_tool_name(&name));
        assert!(!is_provider_tool_name("echo.reverse"));
        assert!(!is_provider_tool_name(""));
        assert!(!is_provider_tool_name(&"a".repeat(MAX_TOOL_NAME_LEN + 1)));
        assert!(is_provider_tool_name(&"a".repeat(MAX_TOOL_NAME_LEN)));
        assert_eq!(
            plugin_tool_display_name(&name).as_deref(),
            Some("echo.reverse")
        );
        assert_eq!(plugin_tool_display_name("read_file"), None);

        let too_long = chatty_wasm_runtime::ToolDefinition {
            name: "t".repeat(60),
            description: String::new(),
            parameters_schema: String::new(),
        };
        let err = tool_def("echo", too_long).unwrap_err().to_string();
        assert!(err.contains("not a legal tool name"), "{err}");
    }

    /// PL-U2 §4: a call asks first only when a grant is side-effecting —
    /// decided by the grants, never by what the tool is called.
    #[test]
    fn approval_follows_the_grants_not_the_tool_name() {
        let grants = |list: &[&str]| list.iter().map(|g| g.to_string()).collect::<Vec<_>>();
        assert!(!plugin_needs_approval(&grants(&[])));
        assert!(!plugin_needs_approval(&grants(&[
            "llm", "config", "logging"
        ])));
        assert!(plugin_needs_approval(&grants(&["llm", "http"])));
        assert!(plugin_needs_approval(&grants(&["file-write"])));
    }

    /// The module's `[resources]` and the spec's `limits` can only lower
    /// the host's limits, and nothing passes the ceilings.
    #[test]
    fn limits_are_only_ever_lowered() {
        let manifest = ModuleManifest::from_str(
            "[module]\nname = \"m\"\nversion = \"1.0.0\"\nwasm = \"m.wasm\"\n\n\
             [resources]\nmax_memory_mb = 32\nmax_execution_ms = 5000\n",
            std::path::Path::new("/modules/m/module.toml"),
        )
        .unwrap();
        let mut spec = plugin("m");
        assert_eq!(limits_for(&manifest, &spec).max_execution_ms, 5000);
        spec.limits = PluginLimits {
            max_memory_mb: Some(1024),
            max_execution_ms: Some(200),
        };
        let limits = limits_for(&manifest, &spec);
        assert_eq!(limits.max_execution_ms, 200);
        assert_eq!(
            limits.max_memory_bytes,
            32 * 1024 * 1024,
            "a spec cannot raise it"
        );
    }

    // -- loading and calling --------------------------------------------------

    /// The echo plugin loads from a module directory by its module name and
    /// its `reverse` tool runs in-process.
    #[tokio::test(flavor = "multi_thread")]
    async fn echo_plugin_reverse_runs_in_process() {
        let echo = load_one(plugin("echo")).await.expect("echo loads");
        let names: Vec<_> = echo.tools.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["echo__echo", "echo__reverse", "echo__count_words"]);
        let reverse = tool_of(&echo, "echo__reverse");
        assert_eq!(reverse.definition().tool, "reverse");
        let out = reverse
            .call(serde_json::json!({ "input": "hello" }))
            .await
            .expect("reverse runs");
        assert_eq!(out, "olleh");
    }

    /// A plugin the host does not have, or has at another version, fails
    /// the load with a reason naming it.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_plugin_that_does_not_resolve_fails_the_load() {
        let err = load_one(plugin("no-such-module"))
            .await
            .err()
            .expect("an unknown module fails")
            .to_string();
        assert!(err.contains("no module named `no-such-module`"), "{err}");

        let mut wrong_version = plugin("echo");
        wrong_version.version = Some("^0.3".to_string());
        let err = format!(
            "{:#}",
            load_one(wrong_version)
                .await
                .err()
                .expect("0.2.0 is not ^0.3")
        );
        assert!(err.contains("the spec asks for ^0.3"), "{err}");

        let mut right_version = plugin("echo");
        right_version.version = Some("^0.2".to_string());
        load_one(right_version).await.expect("0.2.0 is ^0.2");
    }

    /// A `spin` plugin's tool runs past its (spec-lowered) deadline: the
    /// call fails with the deadline as its reason, named after the tool, in
    /// the `Error:` form the stream recognises as a failure. The deadline is
    /// kept well under the time `spin` takes to burn the default fuel budget
    /// (a few hundred ms), or fuel would end the call first.
    #[tokio::test(flavor = "multi_thread")]
    async fn spin_plugin_deadline_is_a_tool_error_with_its_reason() {
        let mut spec = plugin("spin");
        spec.limits.max_execution_ms = Some(50);
        let spin = load_one(spec).await.expect("spin loads");
        let tool = tool_of(&spin, "spin__spin");
        let err = tool
            .call(serde_json::json!({}))
            .await
            .expect_err("spin never returns");
        let message = map_tool_error("spin__spin", err).to_string();
        assert!(message.starts_with("Error: spin__spin:"), "{message}");
        assert!(message.contains("deadline exceeded"), "{message}");

        // The instance was dropped on the interrupt and comes back.
        let err = tool.call(serde_json::json!({})).await.unwrap_err();
        assert!(err.to_string().contains("deadline exceeded"), "{err}");
    }

    /// A plugin granted a side-effecting capability asks before every call;
    /// with no one to ask, the call is refused without running.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_side_effecting_grant_needs_an_approver() {
        let mut spec = plugin("echo");
        spec.grants = vec!["http".to_string()];
        let echo = load_one(spec).await.expect("echo loads");
        let err = tool_of(&echo, "echo__reverse")
            .call(serde_json::json!({ "input": "x" }))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("needs approval"), "{err}");
    }

    // -- inside a turn --------------------------------------------------------

    /// One spec turn through a real [`AgentSession`] against `provider`:
    /// the events it emitted and the session after `finish_turn`.
    async fn run_turn(
        spec: AgentSpec,
        model_config: ModelConfig,
        provider: ProviderConfig,
    ) -> (Vec<SessionEvent>, AgentSession) {
        let _ = crate::init_repositories();
        let workspace = tempfile::tempdir().expect("a workspace");
        let settings = ExecutionSettingsModel {
            workspace_dir: Some(workspace.path().to_string_lossy().into_owned()),
            fetch_enabled: false,
            ..ExecutionSettingsModel::default()
        };
        let built = AgentBuildContext::from_spec(
            &spec,
            AgentServices {
                exec_settings: Some(settings.clone()),
                plugin_host: PluginHost {
                    models: vec![model_config.clone()],
                    providers: vec![provider.clone()],
                    ..host()
                },
                ..AgentServices::default()
            },
        )
        .expect("the spec builds");
        let mut session = AgentSession::new(AgentSessionConfig {
            execution_settings: settings,
            surface: StreamSurface::Headless,
            loop_guard: false,
        });
        session
            .create_conversation(
                "plugins".to_string(),
                "Plugins".to_string(),
                &model_config,
                &provider,
                built.context,
            )
            .await
            .expect("the agent builds with its plugins");

        let events: Rc<RefCell<Vec<SessionEvent>>> = Rc::default();
        let sink = events.clone();
        session
            .begin_turn(TurnInput::text("go"), move |event| {
                sink.borrow_mut().push(event)
            })
            .expect("the turn starts")
            .await;
        let events = Rc::try_unwrap(events).unwrap().into_inner();
        for event in &events {
            session.apply(event);
        }
        session.finish_turn(None, vec![]);
        (events, session)
    }

    fn spec_with(plugins: Vec<PluginSpec>, profile: Option<&str>) -> AgentSpec {
        let mut spec = AgentSpec::named("plugin-user");
        spec.tools.profile = profile.map(str::to_string);
        spec.plugins = plugins;
        spec
    }

    fn ollama(daemon: &FakeDaemon) -> ProviderConfig {
        ProviderConfig::new("Fake".to_string(), ProviderType::Ollama)
            .with_base_url(daemon.base_url())
    }

    fn tool_names(request: &RecordedRequest) -> Vec<String> {
        request.json()["tools"]
            .as_array()
            .map(|tools| {
                tools
                    .iter()
                    .filter_map(|t| t["function"]["name"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn no_error(events: &[SessionEvent]) {
        if let Some(SessionEvent::Error(error)) =
            events.iter().find(|e| matches!(e, SessionEvent::Error(_)))
        {
            panic!("the turn failed: {error:?}");
        }
    }

    /// PL-U2's first verify line, in-process: a spec listing the echo
    /// plugin gets `echo__reverse` as a tool of its own, the model
    /// calls it, and the reversed string is the tool result it reads next.
    /// A `reviewer` profile does not take the plugin away: the spec is the
    /// plugin's allow-list.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_spec_plugin_tool_is_called_inside_the_turn() {
        let key = "plugin-echo-model";
        let daemon = FakeDaemon::scripted(Script::new().route(
            key,
            [
                Reply::tool_call("echo__reverse", serde_json::json!({ "input": "hello" })),
                Reply::text("It is olleh."),
            ],
        ));
        let (events, session) = run_turn(
            spec_with(vec![plugin("echo")], Some("reviewer")),
            model(ProviderType::Ollama, key),
            ollama(&daemon),
        )
        .await;
        no_error(&events);

        let requests = daemon.requests();
        let tools = tool_names(&requests[0]);
        assert!(tools.contains(&"echo__reverse".to_string()), "{tools:?}");
        assert!(
            events.iter().any(|e| matches!(
                e,
                SessionEvent::ToolCallResult { result, .. } if result.contains("olleh")
            )),
            "the tool result is the reversed string"
        );
        assert!(
            String::from_utf8_lossy(&requests[1].body).contains("olleh"),
            "the model reads the result on its next request"
        );
        let trace = session.conversation().unwrap().token_usage();
        assert_eq!(trace.message_usages.len(), 1, "no plugin spent anything");

        // Without the plugin in the spec, the same profile has no such tool.
        let daemon = FakeDaemon::scripted(Script::new().route(key, [Reply::text("ok")]));
        run_turn(
            spec_with(vec![], Some("reviewer")),
            model(ProviderType::Ollama, key),
            ollama(&daemon),
        )
        .await;
        let tools = tool_names(&daemon.requests()[0]);
        assert!(!tools.iter().any(|t| t.contains("__")), "{tools:?}");
    }

    /// PL-U2's deadline line: a `spin` plugin's tool runs out its deadline,
    /// the model is told why, and the turn goes on to an answer.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_plugin_deadline_reaches_the_model_and_the_turn_continues() {
        let key = "plugin-spin-model";
        let daemon = FakeDaemon::scripted(Script::new().route(
            key,
            [
                Reply::tool_call("spin__spin", serde_json::json!({})),
                Reply::text("The spin tool timed out; moving on."),
            ],
        ));
        let mut spin = plugin("spin");
        spin.limits.max_execution_ms = Some(50);
        let (events, _session) = run_turn(
            spec_with(vec![spin], Some("coordinator")),
            model(ProviderType::Ollama, key),
            ollama(&daemon),
        )
        .await;
        no_error(&events);

        let failure = events
            .iter()
            .find_map(|e| match e {
                SessionEvent::ToolCallError { error, .. } => Some(error.clone()),
                SessionEvent::ToolCallResult { result, .. } if result.starts_with("Error:") => {
                    Some(result.clone())
                }
                _ => None,
            })
            .expect("the spin call failed");
        assert!(failure.contains("spin__spin"), "{failure}");
        assert!(failure.contains("deadline exceeded"), "{failure}");
        let requests = daemon.requests();
        assert_eq!(requests.len(), 2, "the turn went on after the error");
        assert!(
            String::from_utf8_lossy(&requests[1].body).contains("deadline exceeded"),
            "the model reads why the call failed"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, SessionEvent::Text(text) if text.contains("moving on")))
        );
    }

    /// PL-U2's usage line: a plugin's `llm::complete` call is part of the
    /// turn's totals as a line of its own, attributed to the plugin and
    /// naming the model that served it and when — never a price of its own.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_plugins_llm_usage_is_its_own_line_in_the_turns_totals() {
        let key = "plugin-ask-model";
        let daemon = FakeDaemon::scripted(Script::new().route(
            key,
            [
                Reply::tool_call("slow-host__ask", serde_json::json!({ "input": "2+2?" })),
                // The plugin's own request, on the calling agent's model.
                Reply::Usage {
                    input: 70,
                    output: 30,
                    cache_read: 0,
                },
                Reply::text("4"),
                Reply::text("The plugin says 4."),
            ],
        ));
        let (events, session) = run_turn(
            spec_with(vec![plugin("slow-host")], Some("coordinator")),
            model(ProviderType::Ollama, key),
            ollama(&daemon),
        )
        .await;
        no_error(&events);
        assert!(events.iter().any(|e| matches!(
            e,
            SessionEvent::ToolCallResult { result, .. } if result.contains('4')
        )));

        let plugin_lines: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                SessionEvent::PluginUsage(line) => Some(line),
                _ => None,
            })
            .collect();
        assert_eq!(plugin_lines.len(), 1, "one line for the one plugin");
        let usage = session.conversation().unwrap().token_usage();
        let line = usage
            .message_usages
            .iter()
            .find(|line| line.plugin.as_deref() == Some("slow-host"))
            .expect("the plugin's line is on the conversation");
        assert_eq!((line.input_tokens, line.output_tokens), (70, 30));
        assert_eq!(
            line.model,
            Some(ModelRef {
                provider: ProviderType::Ollama,
                model_id: key.to_string(),
            })
        );
        assert!(line.at.is_some(), "the line says when it was spent");
        let own = usage.last_usage().expect("the turn's own line");
        assert_eq!(own.plugin, None, "the last line is the agent's own");
        // Two agent requests at the kit's default (10 in, 5 out) plus the
        // plugin's one.
        assert_eq!(usage.total_input_tokens, 10 + 10 + 70);
        assert_eq!(usage.total_output_tokens, 5 + 5 + 30);
    }

    /// The `__` form against each provider's tool-name rule, on fake
    /// servers that refuse a request the way the real ones do: an
    /// OpenAI-wire server (OpenRouter, Azure OpenAI) answers a tool name
    /// outside `^[a-zA-Z0-9_-]{1,64}$` with a 400, and Ollama is held to the
    /// same rule here (it accepts more). A dot would fail all three.
    #[tokio::test(flavor = "multi_thread")]
    async fn plugin_tool_names_pass_each_providers_tool_name_rule() {
        struct ToolNameRule {
            ollama: bool,
        }
        impl Respond for ToolNameRule {
            fn respond(&self, request: &Request) -> ResponseTemplate {
                let rule = regex::Regex::new("^[a-zA-Z0-9_-]{1,64}$").unwrap();
                let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
                let names: Vec<&str> = body["tools"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|t| t["function"]["name"].as_str())
                    .collect();
                if let Some(bad) = names.iter().find(|n| !rule.is_match(n)) {
                    return ResponseTemplate::new(400).set_body_json(serde_json::json!({
                        "error": {
                            "message": format!("Invalid 'tools[].function.name': '{bad}'"),
                            "type": "invalid_request_error",
                        }
                    }));
                }
                if !names.iter().any(|n| n.contains("__")) {
                    return ResponseTemplate::new(500).set_body_string("no plugin tool sent");
                }
                if self.ollama {
                    let record = serde_json::json!({
                        "model": "m", "created_at": "2026-01-01T00:00:00Z",
                        "message": { "role": "assistant", "content": "ok" },
                        "done": true, "done_reason": "stop",
                        "prompt_eval_count": 1, "eval_count": 1,
                    });
                    ResponseTemplate::new(200)
                        .set_body_raw(format!("{record}\n"), "application/x-ndjson")
                } else {
                    ResponseTemplate::new(200).set_body_raw(
                        sse_stream(&[
                            serde_json::json!({ "id": "x", "model": "m", "choices": [{
                                "index": 0, "delta": { "role": "assistant", "content": "ok" },
                                "finish_reason": null }] }),
                            serde_json::json!({ "id": "x", "model": "m", "choices": [{
                                "index": 0, "delta": {}, "finish_reason": "stop" }],
                                "usage": { "prompt_tokens": 1, "completion_tokens": 1,
                                    "total_tokens": 2 } }),
                        ]),
                        "text/event-stream",
                    )
                }
            }
        }

        for provider_type in [
            ProviderType::OpenRouter,
            ProviderType::AzureOpenAI,
            ProviderType::Ollama,
        ] {
            let server = MockServer::start().await;
            Mock::given(wiremock::matchers::method("POST"))
                .respond_with(ToolNameRule {
                    ollama: provider_type == ProviderType::Ollama,
                })
                .mount(&server)
                .await;
            let provider = ProviderConfig::new("Fake".to_string(), provider_type.clone())
                .with_api_key("sk-fake".to_string())
                .with_base_url(server.uri());
            let (events, _) = run_turn(
                spec_with(vec![plugin("echo")], Some("coordinator")),
                model(provider_type.clone(), "m"),
                provider,
            )
            .await;
            no_error(&events);
            assert!(
                events
                    .iter()
                    .any(|e| matches!(e, SessionEvent::Text(text) if text == "ok")),
                "{provider_type:?} accepted the plugin tool names"
            );
            // The same server refuses the dotted form.
            let dotted = reqwest::Client::new()
                .post(server.uri())
                .json(&serde_json::json!({ "tools": [
                    { "type": "function", "function": { "name": "echo.reverse" } }
                ] }))
                .send()
                .await
                .expect("the fake answers");
            assert_eq!(dotted.status(), 400, "{provider_type:?} refuses a dot");
        }
    }
}
