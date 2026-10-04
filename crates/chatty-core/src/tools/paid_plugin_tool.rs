//! A paid plugin's tools, built from its lockfile manifest and run by hive
//! (ADR-0024 §§ 1–2, MK-2).
//!
//! A paid plugin's WASM runs only in hive's module runtime: never in a
//! worker, on a desktop or in the local gateway. So a spec plugin the host
//! lists in [`PluginHost::paid`](super::plugin_tool::PluginHost::paid) is
//! not resolved under the module roots or compiled: its tools come from
//! the published version's [`PaidPluginManifest`] — names, descriptions and
//! argument schemas — and each call is one `module.call` over the worker's
//! connection to its broker, which the broker host runs there.
//!
//! * **Names** are a plugin tool's, `<plugin>__<tool>`, checked the same
//!   way ([`tool_def`]).
//! * **Where.** Only an agent with a fabric transport — a worker on its
//!   broker's connection — has one to call over; any other agent that lists
//!   a paid plugin fails to build.
//! * **Money refusals** (`needs_acceptance`, `fee_refused`) are the user's
//!   to resolve, never the model's: the tool error carries the typed text,
//!   is not retryable, and the worker's task fails with it
//!   ([`chatty_fabric::find_money_refusal`]), so every caller up the tree
//!   passes it on and the root shows it.

use std::sync::Arc;

use anyhow::{Result, bail};
use chatty_fabric::{
    CallError, CallEvent, CallRequest, CallResult, FEE_REFUSED, LockedPlugin, ModuleCallOutcome,
    ModuleCallParams, PaidPluginManifest, Transport,
};
use futures::StreamExt;
use rig_agent::tool::{DynamicTool, ToolOutput};

use super::plugin_tool::{PluginToolDef, tool_def};
use super::{ToolError, map_tool_error};
use crate::agent_spec::PluginSpec;

/// One paid plugin of one agent: its locked version and its tools.
pub struct PaidPlugin {
    pub name: String,
    pub description: String,
    pub plugin: LockedPlugin,
    pub tools: Vec<PluginToolDef>,
    transport: Arc<dyn Transport>,
}

impl PaidPlugin {
    /// `manifest`'s tools, called over `transport`.
    pub fn new(manifest: &PaidPluginManifest, transport: Arc<dyn Transport>) -> Result<Self> {
        let name = manifest.plugin.module.clone();
        let tools = manifest
            .tools
            .iter()
            .map(|tool| {
                tool_def(
                    &name,
                    tool.name.clone(),
                    tool.description.clone(),
                    &tool.parameters_schema,
                )
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Self {
            name,
            description: manifest.description.clone(),
            plugin: manifest.plugin.clone(),
            tools,
            transport,
        })
    }
}

/// The paid plugins among `specs`: each one `paid` has a manifest for,
/// built over `transport`, sorted by name. A paid plugin with no transport
/// to call over fails the build: its code runs only on hive.
pub fn paid_plugins(
    specs: &[PluginSpec],
    paid: &[PaidPluginManifest],
    transport: Option<&Arc<dyn Transport>>,
) -> Result<Vec<PaidPlugin>> {
    let mut plugins = Vec::new();
    for spec in specs {
        let Some(manifest) = paid.iter().find(|m| m.plugin.module == spec.module) else {
            continue;
        };
        let Some(transport) = transport else {
            bail!(
                "plugin `{}` is paid: it runs only on hive, from an agent connected to its broker",
                spec.module
            );
        };
        plugins.push(PaidPlugin::new(manifest, Arc::clone(transport))?);
    }
    plugins.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(plugins)
}

/// One tool of a paid plugin, as a rig tool of the agent.
#[derive(Clone)]
pub struct PaidPluginTool {
    def: PluginToolDef,
    plugin: LockedPlugin,
    transport: Arc<dyn Transport>,
}

impl PaidPluginTool {
    /// Every tool of `plugin`.
    pub fn all(plugin: &PaidPlugin) -> Vec<Self> {
        plugin
            .tools
            .iter()
            .map(|def| Self {
                def: def.clone(),
                plugin: plugin.plugin.clone(),
                transport: Arc::clone(&plugin.transport),
            })
            .collect()
    }

    pub fn definition(&self) -> &PluginToolDef {
        &self.def
    }

    /// Send the call as a `module.call` and wait for its one answer. The
    /// worker's connection names the run it is made from.
    pub async fn call(&self, args: serde_json::Value) -> Result<String, ToolError> {
        let arguments = match args {
            serde_json::Value::Null => "{}".to_string(),
            other => other.to_string(),
        };
        let request = CallRequest::ModuleCall(ModuleCallParams {
            plugin: self.plugin.clone(),
            tool: self.def.tool.clone(),
            arguments,
            run: None,
        });
        let failed = |error: CallError| ToolError::OperationFailed(error.to_string());
        let mut stream = self.transport.call(request).await.map_err(failed)?;
        while let Some(event) = stream.next().await {
            match event.map_err(failed)? {
                CallEvent::Result(CallResult::ModuleCalled(outcome)) => {
                    return match outcome {
                        ModuleCallOutcome::Result { content } => Ok(content),
                        ModuleCallOutcome::ToolError { message } => {
                            Err(ToolError::OperationFailed(message))
                        }
                    };
                }
                CallEvent::Result(other) => {
                    return Err(ToolError::OperationFailed(format!(
                        "module.call answered with {other:?}"
                    )));
                }
                CallEvent::Progress(_)
                | CallEvent::Ask { .. }
                | CallEvent::InputWithdrawn { .. }
                | CallEvent::Approve { .. }
                | CallEvent::Swarm(_) => {}
            }
        }
        Err(ToolError::OperationFailed(
            "module.call ended without an answer".to_string(),
        ))
    }

    /// The rig tool: errors through [`map_tool_error`] under the advertised
    /// name, a money refusal terminal for the model.
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
                tool.call(args).await.map(ToolOutput::text).map_err(|e| {
                    let money = chatty_fabric::find_money_refusal(&e.to_string()).is_some();
                    let mapped = map_tool_error(&tool.def.name, e);
                    if money {
                        mapped.with_retryable(false).with_code(FEE_REFUSED)
                    } else {
                        mapped
                    }
                })
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use chatty_fabric::{FeeRefusalReason, ItemKind, ItemRef, PaidTool};
    use futures::stream;

    use super::*;
    use crate::tools::plugin_tool::{PluginHost, load_plugins};

    /// Records every request; answers a `module.call` with `answer`.
    struct Broker {
        seen: Mutex<Vec<CallRequest>>,
        answer: Result<ModuleCallOutcome, CallError>,
    }

    #[async_trait::async_trait]
    impl Transport for Broker {
        async fn call(&self, req: CallRequest) -> Result<chatty_fabric::CallStream, CallError> {
            self.seen.lock().unwrap().push(req);
            let item = self
                .answer
                .clone()
                .map(|outcome| CallEvent::Result(CallResult::ModuleCalled(outcome)));
            Ok(stream::iter([item]).boxed())
        }
    }

    fn manifest() -> PaidPluginManifest {
        PaidPluginManifest {
            plugin: LockedPlugin {
                module: "ocr".into(),
                version: "1.2.0".into(),
                sha256: "ab".repeat(32),
            },
            description: "Reads scanned pages".into(),
            tools: vec![PaidTool {
                name: "read".into(),
                description: "Read one page".into(),
                parameters_schema: r#"{"type":"object","properties":{"page":{"type":"integer"}}}"#
                    .into(),
            }],
        }
    }

    fn spec(module: &str) -> PluginSpec {
        PluginSpec {
            module: module.into(),
            ..PluginSpec::default()
        }
    }

    /// MK-2: a paid plugin's tools come from its lockfile manifest alone.
    /// The host has no module directory at all — nothing to resolve, read
    /// or compile — and the plugin still gets its tools, which call it by
    /// `module.call` over the worker's connection, naming the locked
    /// version; `load_plugins` passes it over rather than looking for its
    /// bytes. With no connection the build fails: it runs only on hive.
    #[tokio::test]
    async fn paid_plugin_tool_built_from_manifest_never_fetches_wasm() {
        let empty = tempfile::tempdir().unwrap();
        let host = PluginHost {
            module_roots: vec![empty.path().to_path_buf()],
            paid: vec![manifest()],
            ..PluginHost::default()
        };
        let specs = [spec("ocr")];
        let model = crate::settings::models::models_store::ModelConfig::new(
            "m".into(),
            "m".into(),
            crate::settings::models::providers_store::ProviderType::Ollama,
            "m".into(),
        );
        assert!(
            load_plugins(&specs, &host, &model).await.unwrap().is_empty(),
            "a paid plugin is never loaded from disk"
        );
        assert_eq!(
            std::fs::read_dir(empty.path()).unwrap().count(),
            0,
            "nothing was fetched into the module roots"
        );

        let broker = Arc::new(Broker {
            seen: Mutex::new(Vec::new()),
            answer: Ok(ModuleCallOutcome::Result {
                content: "page one".into(),
            }),
        });
        let transport: Arc<dyn Transport> = broker.clone();
        let plugins = paid_plugins(&specs, &host.paid, Some(&transport)).unwrap();
        let [plugin] = &plugins[..] else {
            panic!("one paid plugin: {}", plugins.len());
        };
        let tools = PaidPluginTool::all(plugin);
        let [tool] = &tools[..] else {
            panic!("one tool");
        };
        assert_eq!(tool.definition().name, "ocr__read");
        assert_eq!(
            tool.definition().parameters["properties"]["page"]["type"],
            "integer"
        );

        let answer = tool.call(serde_json::json!({"page": 1})).await.unwrap();
        assert_eq!(answer, "page one");
        let seen = broker.seen.lock().unwrap();
        assert_eq!(
            *seen,
            [CallRequest::ModuleCall(ModuleCallParams {
                plugin: manifest().plugin,
                tool: "read".into(),
                arguments: r#"{"page":1}"#.into(),
                run: None,
            })]
        );
        drop(seen);

        let err = paid_plugins(&specs, &host.paid, None)
            .err()
            .expect("no transport, no paid plugin");
        assert!(err.to_string().contains("runs only on hive"), "{err}");
    }

    /// A tool error is the model's to read; a money refusal is not, and
    /// keeps its typed text.
    #[tokio::test]
    async fn a_fee_refusal_is_a_typed_tool_error() {
        let refused = CallError::FeeRefused {
            item: ItemRef {
                kind: ItemKind::Plugin,
                name: "ocr".into(),
                version: "1.2.0".into(),
            },
            reason: FeeRefusalReason::Funding,
            resets_at: None,
        };
        let transport: Arc<dyn Transport> = Arc::new(Broker {
            seen: Mutex::new(Vec::new()),
            answer: Err(refused.clone()),
        });
        let plugin = PaidPlugin::new(&manifest(), transport).unwrap();
        let tool = PaidPluginTool::all(&plugin).remove(0);
        let err = tool.call(serde_json::Value::Null).await.unwrap_err();
        assert_eq!(err.to_string(), refused.to_string());
        assert_eq!(
            chatty_fabric::find_money_refusal(&err.to_string()),
            Some("fee_refused: plugin ocr@1.2.0: funding")
        );
    }
}
