//! What the model can address, and whose machine each one runs on
//! (ADR-0011 C5).
//!
//! Two sources, one list. The static half is settings: remote A2A agents the
//! user configured, and the agent specs the broker serves (PL-U5: one kind
//! of local agent, a spec run by the harness; a WASM plugin is a tool inside
//! one, never an agent). The live half is the broker's own aggregated card,
//! read over HTTP or the fabric — a worker that registered a minute ago is
//! addressable and so belongs here, and only the broker knows it exists.
//!
//! Every entry carries an [`AgentOrigin`], because "voucher-agent" and
//! "local-agent" read the same to a model and one of them is somebody else's
//! server.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use rig_agent::tool::{Tool, ToolContext, ToolExecutionError};
use serde::{Deserialize, Serialize};

use crate::agent_spec::{AgentSpec, Grant, load_agent_spec_from};
use crate::services::lazy_broker::LazyBroker;
use crate::settings::models::a2a_store::A2aAgentConfig;
use crate::tools::ToolError;
use chatty_fabric::{AgentOrigin, CallEvent, CallRequest, Transport};

/// Arguments for listing A2A agents (no arguments needed)
#[derive(Deserialize, Serialize)]
pub struct ListAgentsToolArgs {}

/// Summary of a single configured remote A2A agent, safe for display to the LLM.
#[derive(Debug, Serialize, Clone)]
pub struct A2aAgentSummary {
    pub name: String,
    pub url: String,
    /// `true` if an API key is configured (value is never exposed).
    pub has_api_key: bool,
    pub enabled: bool,
    /// Skills advertised by the agent card (may be empty if not yet fetched).
    pub skills: Vec<String>,
}

/// One of the broker's local-worker agents, when the gateway publishes
/// any (ADR-0011 C2; several, by name, under C10).
#[derive(Debug, Serialize, Clone)]
pub struct LocalWorkerAgentSummary {
    pub name: String,
    pub description: String,
}

/// One agent the model can address, with where it runs.
///
/// The shape the model sees is flat and uniform on purpose: the interesting
/// question about an agent is not which of four buckets it came from, it is
/// what it is called, what it does, and whose machine it runs on.
#[derive(Debug, Serialize, Clone, PartialEq)]
pub struct AgentListing {
    pub name: String,
    /// Whose machine it runs on. See [`AgentOrigin`].
    pub origin: AgentOrigin,
    /// `remote` or `worker` — how to think about what it is, not where it
    /// is.
    pub kind: &'static str,
    pub description: String,
    /// Present for a configured remote agent; the broker's own agents are
    /// addressed by name, not URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// `false` only for a remote agent the user disabled.
    pub enabled: bool,
    /// Skills or tools it advertises, when it says.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<String>,
    /// `true` if an API key is configured for it (the value is never exposed).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub has_api_key: bool,
    /// For a local agent with a spec: its plugins and what each may reach.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub plugins: Vec<PluginGrants>,
}

/// One plugin of a local agent's spec, as its card shows it (PL-U4): what
/// the agent granted it. A capability it was not granted is refused when
/// the plugin calls it.
#[derive(Debug, Serialize, Clone, PartialEq)]
pub struct PluginGrants {
    pub module: String,
    /// Granted capabilities by WIT name (`file:<root>` for a file root),
    /// always with `logging`.
    pub grants: Vec<String>,
}

impl PluginGrants {
    fn of(spec: &AgentSpec) -> Vec<Self> {
        spec.plugins
            .iter()
            .map(|plugin| {
                let grants = std::iter::once(Grant::Logging)
                    .chain(plugin.grants.iter().cloned())
                    .collect::<std::collections::BTreeSet<_>>()
                    .into_iter()
                    .map(|grant| grant.to_string())
                    .collect();
                Self {
                    module: plugin.module.clone(),
                    grants,
                }
            })
            .collect()
    }
}

/// Output from the list_agents tool
#[derive(Debug, Serialize)]
pub struct ListAgentsToolOutput {
    /// Every addressable agent, in name order, each carrying its origin.
    pub agents: Vec<AgentListing>,
    pub total: usize,
    pub note: String,
}

/// Tool that lists all available agents: remotely configured A2A agents and the
/// agent specs the broker serves.
///
/// This gives the LLM visibility into what agents are available, including their
/// names, URLs/types, and skills. Each agent is invokable via `invoke_agent`
/// and the `/agent <name> <prompt>` command.
#[derive(Clone)]
pub struct ListAgentsTool {
    /// Snapshot of configured remote A2A agents taken at construction time.
    remote_agents: Vec<A2aAgentConfig>,
    /// The broker's local workers (ADR-0011 C2, named under C10), if the
    /// gateway is running.
    local_workers: Vec<LocalWorkerAgentSummary>,
    /// Where to read the broker's live participant table, when the gateway is
    /// running (ADR-0011 C5).
    gateway_base_url: Option<String>,
    /// A broker that has not necessarily started yet (BI-2, AGE-634).
    /// Consulted only when `gateway_base_url` is `None`.
    lazy_broker: Option<Arc<dyn LazyBroker>>,
    /// The fabric the broker's directory is read through (ADR-0020, BI-4):
    /// a worker's broker-made connection. When present, the live half is
    /// never read over loopback HTTP.
    transport: Option<Arc<dyn Transport>>,
    /// Where a local agent's spec is looked up for its card's plugins:
    /// the workspace and the data directory (then the presets).
    spec_workspace: Option<PathBuf>,
    spec_data_dir: Option<PathBuf>,
    http: reqwest::Client,
}

impl ListAgentsTool {
    pub fn new(remote_agents: Vec<A2aAgentConfig>) -> Self {
        Self {
            remote_agents,
            local_workers: Vec::new(),
            gateway_base_url: None,
            lazy_broker: None,
            transport: None,
            spec_workspace: None,
            spec_data_dir: dirs::data_dir(),
            http: reqwest::Client::new(),
        }
    }

    /// Read the broker's live participant table as well as settings
    /// (ADR-0011 C5). Without it, this lists only what was configured.
    pub fn with_gateway_port(mut self, port: u16) -> Self {
        self.gateway_base_url = Some(format!("http://localhost:{port}"));
        self
    }

    /// Start the broker itself on first use instead of expecting it already
    /// running (BI-2, AGE-634). Ignored when a `gateway_port` was already
    /// given: that path is for tests and hosts that already know a live
    /// port.
    pub fn with_lazy_broker(mut self, broker: Arc<dyn LazyBroker>) -> Self {
        self.lazy_broker = Some(broker);
        self
    }

    /// Read the broker's directory over `transport` instead of its
    /// aggregated card over loopback HTTP (ADR-0020, BI-4).
    pub fn with_transport(mut self, transport: Arc<dyn Transport>) -> Self {
        self.transport = Some(transport);
        self
    }

    /// Look local agents' specs up under `workspace` and `data_dir` (then
    /// the presets) for the plugins their cards show. Without it: the
    /// platform data directory, then the presets.
    pub fn with_spec_dirs(mut self, workspace: Option<PathBuf>, data_dir: Option<PathBuf>) -> Self {
        self.spec_workspace = workspace;
        self.spec_data_dir = data_dir;
        self
    }

    /// The fabric the directory is read through: the one this tool was
    /// given, else the lazy broker's direct handle (starting it).
    ///
    /// `Ok(None)` means no broker is configured at all — nothing to report.
    /// `Err` means one is configured but failed to start (AGE-746, e.g. a
    /// port collision): the caller surfaces this to the user rather than
    /// treating it the same as "no broker".
    async fn fabric_transport(&self) -> Result<Option<Arc<dyn Transport>>, String> {
        if let Some(transport) = &self.transport {
            return Ok(Some(transport.clone()));
        }
        let Some(broker) = self.lazy_broker.as_ref() else {
            return Ok(None);
        };
        broker.transport().await.map_err(|error| {
            tracing::warn!(%error, "Failed to start the broker for list_agents");
            error.to_string()
        })
    }

    /// The gateway's base URL, resolving a [`LazyBroker`] on first use if
    /// that is all this tool has. See [`Self::fabric_transport`] for what
    /// `Ok(None)` vs `Err` means.
    async fn gateway_base_url(&self) -> Result<Option<String>, String> {
        if let Some(url) = &self.gateway_base_url {
            return Ok(Some(url.clone()));
        }
        let Some(broker) = self.lazy_broker.as_ref() else {
            return Ok(None);
        };
        broker.ensure_started().await.map(Some).map_err(|error| {
            tracing::warn!(%error, "Failed to start the broker for list_agents");
            error.to_string()
        })
    }

    /// Advertise the broker's local workers: chatty agents in their own
    /// process (ADR-0011 C2), one per name (C10). The live card, when the
    /// gateway answers, supplies each one's real description — its model
    /// and tool set — so this is the fallback text for when it does not.
    pub fn with_local_workers<I, S>(mut self, names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.local_workers = names
            .into_iter()
            .map(|name| LocalWorkerAgentSummary {
                name: name.into(),
                description: "A chatty agent in its own process and its own \
                              workspace. Delegate a self-contained task to it."
                    .to_string(),
            })
            .collect();
        self
    }
}

impl Tool for ListAgentsTool {
    const NAME: &'static str = "list_agents";
    type Error = ToolError;
    type Args = ListAgentsToolArgs;
    type Output = ListAgentsToolOutput;

    fn description(&self) -> String {
        "List all available agents: remote A2A agents configured via \
                         Settings → A2A Agents, and locally installed WASM module agents. \
                         Returns each agent's name, type, enabled state, and the skills/tools \
                         it provides. \
                         \n\n\
                         Use this to discover what agents are available before deciding whether \
                         to delegate a task. Agents can be invoked with \
                         `/agent <name> <prompt>` — each agent runs its own full agentic \
                         loop and returns a result."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {},
            "required": []
        })
    }

    /// Keep the real failure text in front of the user and the model:
    /// rig's default `map_error` redacts it to "the tool failed" (AGE-187).
    fn map_error(&self, error: Self::Error) -> ToolExecutionError {
        crate::tools::map_tool_error(Self::NAME, error)
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        _args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        // Name → listing, so the live table can add to what settings knows
        // without listing the same agent twice, and so the model reads them in
        // a stable order.
        let mut listings: BTreeMap<String, AgentListing> = BTreeMap::new();

        for agent in &self.remote_agents {
            listings.insert(
                agent.name.clone(),
                AgentListing {
                    name: agent.name.clone(),
                    origin: AgentOrigin::RemoteConfigured,
                    kind: "remote",
                    description: format!("Remote A2A agent at {}", agent.url),
                    url: Some(agent.url.clone()),
                    enabled: agent.enabled,
                    skills: agent.skills.clone(),
                    has_api_key: agent.has_api_key(),
                    plugins: Vec::new(),
                },
            );
        }

        // The live half. A remote agent the user configured keeps its own
        // entry: the broker would report it as whatever it is to the broker,
        // and what the *user* did is the more informative label.
        let (live, broker_error) = self.live_agents().await;
        for live in live {
            listings.entry(live.name.clone()).or_insert(live);
        }

        // After the live half: the broker's card says what each worker
        // actually runs (its model, its tool set — ADR-0011 C10), and this
        // entry only stands in when the broker did not answer.
        for worker in &self.local_workers {
            listings
                .entry(worker.name.clone())
                .or_insert_with(|| AgentListing {
                    name: worker.name.clone(),
                    origin: AgentOrigin::Local,
                    kind: "worker",
                    description: worker.description.clone(),
                    url: None,
                    enabled: true,
                    skills: Vec::new(),
                    has_api_key: false,
                    plugins: Vec::new(),
                });
        }

        // A local agent's card shows its spec's plugins and their grants
        // (PL-U4): the spec is the authority on what a plugin may reach.
        for listing in listings.values_mut() {
            if listing.origin == AgentOrigin::Local
                && let Ok(loaded) = load_agent_spec_from(
                    &listing.name,
                    self.spec_workspace.as_deref(),
                    self.spec_data_dir.as_deref(),
                )
            {
                listing.plugins = PluginGrants::of(&loaded.spec);
            }
        }

        let agents: Vec<AgentListing> = listings.into_values().collect();
        tracing::info!(
            agent_count = agents.len(),
            live_read = self.gateway_base_url.is_some()
                || self.lazy_broker.is_some()
                || self.transport.is_some(),
            "list_agents called"
        );

        let mut note = if agents.is_empty() {
            "No agents are available. Remote agents can be added via Settings → Extensions; \
             local agents are agent specs in `.chatty/agents/<name>.toml`, served by the \
             broker (Settings → Agents)."
                .to_string()
        } else {
            "To invoke an agent, use the `invoke_agent` tool with the agent's name and a prompt. \
             `origin` says whose machine it runs on: `local` is this machine, `fleet` is a \
             machine this user leased, `remote_configured` is a third-party URL the user \
             configured, `discovered` is a third party nobody chose. Only enabled agents can be \
             called."
                .to_string()
        };
        // AGE-746: a broker that is configured but failed to start (a port
        // collision, most often) used to be indistinguishable from one that
        // was simply never turned on — this tool would list local workers'
        // static fallback descriptions either way. Say so, so the model can
        // tell the user rather than reporting local agents as reachable.
        if let Some(error) = broker_error {
            note = format!(
                "The local broker failed to start ({error}), so live agent data (and any agent \
                 only the broker knows about) is unavailable; entries below may be stale or \
                 unreachable. This is often another chatty process already holding the gateway \
                 port — check Settings → Agents, or close the other process. {note}"
            );
        }

        Ok(ListAgentsToolOutput {
            total: agents.len(),
            agents,
            note,
        })
    }
}

impl ListAgentsTool {
    /// How long the broker gets to answer. It is a local HTTP call to a
    /// process on the same machine; if it is not answering, the agents it
    /// would have listed are not reachable either, so listing without them is
    /// the honest answer rather than a stalled turn.
    const LIVE_READ_TIMEOUT: Duration = Duration::from_millis(750);

    /// The broker's live participant table, from its aggregated agent card,
    /// plus the reason the broker itself could not be reached — not raised
    /// as an error, since a `list_agents` call must still answer with what
    /// settings alone can say, but not swallowed either: a broker that is
    /// merely off is unremarkable, but one that is configured and failed to
    /// bind its port (AGE-746) is worth telling the user about, so the note
    /// carries it back to `call`.
    async fn live_agents(&self) -> (Vec<AgentListing>, Option<String>) {
        match self.fabric_transport().await {
            Ok(Some(transport)) => return (directory_over(transport.as_ref()).await, None),
            Ok(None) => {}
            Err(error) => return (Vec::new(), Some(error)),
        }
        let base = match self.gateway_base_url().await {
            Ok(Some(base)) => base,
            Ok(None) => return (Vec::new(), None),
            Err(error) => return (Vec::new(), Some(error)),
        };
        let url = format!("{base}/.well-known/agent.json");

        let card = match self
            .http
            .get(&url)
            .timeout(Self::LIVE_READ_TIMEOUT)
            .send()
            .await
        {
            Ok(response) if response.status().is_success() => {
                match response.json::<serde_json::Value>().await {
                    Ok(card) => card,
                    Err(error) => {
                        tracing::debug!(%url, ?error, "the broker's card did not parse");
                        return (Vec::new(), None);
                    }
                }
            }
            Ok(response) => {
                tracing::debug!(%url, status = %response.status(), "the broker refused its card");
                return (Vec::new(), None);
            }
            Err(error) => {
                tracing::debug!(%url, ?error, "no broker to list live agents from");
                return (Vec::new(), None);
            }
        };

        let agents = card
            .get("agents")
            .and_then(|agents| agents.as_array())
            .map(|agents| agents.iter().filter_map(listing_from_card).collect())
            .unwrap_or_default();
        (agents, None)
    }
}

/// The broker's directory, read over the fabric: the same card entries its
/// aggregated card lists, as a `list_agents` call's result. A failed call is
/// an empty live half, as a broker that does not answer over HTTP is.
async fn directory_over(transport: &dyn Transport) -> Vec<AgentListing> {
    use futures::StreamExt;

    let mut stream = match transport.call(CallRequest::ListAgents).await {
        Ok(stream) => stream,
        Err(error) => {
            tracing::debug!(%error, "the broker's directory could not be read");
            return Vec::new();
        }
    };
    while let Some(event) = stream.next().await {
        match event {
            Ok(CallEvent::Result(agents)) => {
                return agents
                    .as_array()
                    .map(|agents| agents.iter().filter_map(listing_from_card).collect())
                    .unwrap_or_default();
            }
            Ok(_) => {}
            Err(error) => {
                tracing::debug!(%error, "the broker's directory could not be read");
                return Vec::new();
            }
        }
    }
    Vec::new()
}

/// One entry of the broker's aggregated card as a listing.
///
/// An entry with no name is skipped: it cannot be addressed, so telling the
/// model about it would only invite a call that fails.
fn listing_from_card(card: &serde_json::Value) -> Option<AgentListing> {
    let name = card.get("name")?.as_str()?.to_string();
    // An origin the broker did not state, or one this build does not know, is
    // not treated as trusted — see `AgentOrigin::from_wire`.
    let origin = card
        .get("origin")
        .and_then(|origin| origin.as_str())
        .and_then(AgentOrigin::from_wire)
        .unwrap_or(AgentOrigin::Discovered);
    let description = card
        .get("description")
        .and_then(|text| text.as_str())
        .unwrap_or_default()
        .to_string();
    let skills = card
        .get("skills")
        .and_then(|skills| skills.as_array())
        .map(|skills| {
            skills
                .iter()
                .filter_map(|skill| {
                    skill
                        .get("name")
                        .and_then(|name| name.as_str())
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default();

    Some(AgentListing {
        name,
        origin,
        kind: "worker",
        description,
        url: None,
        enabled: true,
        skills,
        has_api_key: false,
        plugins: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::lazy_broker::LazyBroker;
    use crate::settings::models::a2a_store::A2aAgentConfig;

    /// A broker that is configured but never manages to bind its port —
    /// standing in for the AGE-746 port-collision case.
    struct FailingBroker;

    #[async_trait::async_trait]
    impl LazyBroker for FailingBroker {
        async fn ensure_started(&self) -> anyhow::Result<String> {
            Err(anyhow::anyhow!(
                "failed to bind to 127.0.0.1:8420: Address already in use (os error 98)"
            ))
        }

        fn bound_addrs(&self) -> Vec<std::net::SocketAddr> {
            Vec::new()
        }
    }

    fn make_agent(name: &str, url: &str, enabled: bool) -> A2aAgentConfig {
        A2aAgentConfig {
            name: name.to_string(),
            url: url.to_string(),
            api_key: None,
            enabled,
            skills: vec![],
        }
    }

    async fn list(tool: &ListAgentsTool) -> ListAgentsToolOutput {
        tool.call(&mut ToolContext::new(), ListAgentsToolArgs {})
            .await
            .expect("listing agents does not fail")
    }

    fn find<'a>(output: &'a ListAgentsToolOutput, name: &str) -> &'a AgentListing {
        output
            .agents
            .iter()
            .find(|agent| agent.name == name)
            .unwrap_or_else(|| panic!("{name} is listed, got {:?}", output.agents))
    }

    #[tokio::test]
    async fn nothing_configured_lists_nothing() {
        let output = list(&ListAgentsTool::new(vec![])).await;
        assert_eq!(output.total, 0);
        assert!(output.agents.is_empty());
        assert!(output.note.contains("No agents are available"));
    }

    /// AGE-746: a bind failure (e.g. two chatty processes on the same port)
    /// used to be silent — `list_agents` fell back to local workers' static
    /// descriptions with no sign the broker was actually down. Now the
    /// failure reaches the note the model reads, wherever the port fallback
    /// happened to come from — an `ensure_started` broker with no
    /// pre-resolved URL and no direct transport, the shape a lazily-started
    /// gateway is in until its first call.
    #[tokio::test]
    async fn gateway_bind_failure_is_reported() {
        let tool = ListAgentsTool::new(vec![])
            .with_lazy_broker(Arc::new(FailingBroker))
            .with_local_workers(["coder"]);

        let output = list(&tool).await;

        // The local worker's static fallback entry still lists (a broker
        // that never answers is not a reason to hide it), but the note
        // tells the user the live data behind it could not be reached.
        assert!(find(&output, "coder").enabled);
        assert!(
            output.note.contains("failed to start"),
            "note should mention the broker failed to start: {}",
            output.note
        );
        assert!(
            output.note.contains("Address already in use"),
            "note should carry the real bind error, not a generic message: {}",
            output.note
        );
    }

    /// The issue's "Verify", static half: a configured remote is labelled as
    /// somebody else's, and its key is never in the output.
    #[tokio::test]
    async fn a_configured_remote_is_remote_configured_and_keeps_its_key() {
        let mut agent = make_agent("voucher-agent", "https://hive.dev/a2a/voucher", true);
        agent.api_key = Some("secret-value".to_string());
        agent.skills = vec!["translate".to_string()];

        let output = list(&ListAgentsTool::new(vec![agent])).await;
        let listed = find(&output, "voucher-agent");

        assert_eq!(listed.origin, AgentOrigin::RemoteConfigured);
        assert!(!listed.origin.is_own_fleet());
        assert_eq!(listed.kind, "remote");
        assert_eq!(listed.url.as_deref(), Some("https://hive.dev/a2a/voucher"));
        assert!(listed.enabled);
        assert!(listed.has_api_key);
        assert_eq!(listed.skills, vec!["translate".to_string()]);

        let json = serde_json::to_string(&output.agents).expect("the listing serializes");
        assert!(
            !json.contains("secret-value"),
            "an api key must never reach the model: {json}"
        );
    }

    #[tokio::test]
    async fn a_disabled_remote_is_listed_as_disabled_rather_than_hidden() {
        let output = list(&ListAgentsTool::new(vec![make_agent(
            "off-agent",
            "https://example.com/a2a",
            false,
        )]))
        .await;
        assert!(!find(&output, "off-agent").enabled);
    }

    /// PL-U4: a local agent's card shows its spec's plugins and what each
    /// was granted (always `logging`); an agent without a spec shows none.
    #[tokio::test]
    async fn a_local_agents_card_shows_its_plugins_grants() {
        let workspace = tempfile::tempdir().unwrap();
        let agents = workspace
            .path()
            .join(crate::agent_spec::WORKSPACE_AGENTS_DIR);
        std::fs::create_dir_all(&agents).unwrap();
        std::fs::write(
            agents.join("auditor.toml"),
            "[agent]\nname = \"auditor\"\n\n\
             [[plugins]]\nmodule = \"config-reader\"\ngrants = [\"config\"]\n\n\
             [[plugins]]\nmodule = \"echo\"\n",
        )
        .unwrap();
        let tool = ListAgentsTool::new(vec![])
            .with_local_workers(["auditor", "local-agent"])
            .with_spec_dirs(Some(workspace.path().to_path_buf()), None);

        let output = list(&tool).await;
        assert_eq!(
            find(&output, "auditor").plugins,
            [
                PluginGrants {
                    module: "config-reader".to_string(),
                    grants: vec!["config".to_string(), "logging".to_string()],
                },
                PluginGrants {
                    module: "echo".to_string(),
                    grants: vec!["logging".to_string()],
                },
            ]
        );
        assert!(find(&output, "local-agent").plugins.is_empty());
        let json = serde_json::to_value(&output.agents).unwrap();
        assert_eq!(
            json[0]["plugins"][0],
            serde_json::json!({ "module": "config-reader", "grants": ["config", "logging"] })
        );
        assert!(json[1].get("plugins").is_none(), "{json}");
    }

    /// PL-U5: the roster's specs are this machine's workers, one kind of
    /// local agent; nothing else is listed as local.
    #[tokio::test]
    async fn the_roster_s_specs_are_the_local_agents() {
        let roster = crate::agent_spec::roster_names_from(&[], None, None);
        let tool = ListAgentsTool::new(vec![make_agent("remote", "https://example.com/a2a", true)])
            .with_local_workers(roster.iter().cloned());

        let output = list(&tool).await;
        assert_eq!(output.total, roster.len() + 1);
        for name in &roster {
            assert_eq!(find(&output, name).origin, AgentOrigin::Local);
            assert_eq!(find(&output, name).kind, "worker");
        }
        assert_eq!(find(&output, "benford-analyst").kind, "worker");
        assert!(
            output
                .agents
                .iter()
                .all(|agent| agent.kind == "worker" || agent.kind == "remote"),
            "{:?}",
            output.agents
        );
        assert_eq!(
            find(&output, "remote").origin,
            AgentOrigin::RemoteConfigured
        );
        assert!(
            output.note.contains("origin"),
            "the model is told what the label means"
        );
    }

    #[tokio::test]
    async fn agents_are_listed_in_name_order() {
        let tool = ListAgentsTool::new(vec![
            make_agent("zeta", "https://example.com/z", true),
            make_agent("alpha", "https://example.com/a", true),
        ]);
        let names: Vec<String> = list(&tool)
            .await
            .agents
            .into_iter()
            .map(|agent| agent.name)
            .collect();
        assert_eq!(names, vec!["alpha".to_string(), "zeta".to_string()]);
    }

    // ── The live half ────────────────────────────────────────────────────────

    /// A card the broker would serve, with the origins it would put on it.
    fn broker_card() -> serde_json::Value {
        serde_json::json!({
            "schema_version": "0.1",
            "gateway": true,
            "agents": [
                {
                    "name": "local-agent-0",
                    "description": "A chatty agent in its own process",
                    "skills": [{"name": "delegate"}],
                    "origin": "local",
                },
                {
                    "name": "leased-vm",
                    "description": "A worker in a leased microVM",
                    "origin": "fleet",
                },
                {
                    "name": "mystery",
                    "description": "No origin stated",
                },
            ],
        })
    }

    /// The issue's "Verify", live half: a worker that registered after the
    /// tool was built is listed, as local, without anyone reconfiguring
    /// anything.
    #[tokio::test]
    async fn the_broker_s_live_participants_are_listed_with_their_origin() {
        let (port, _server) = serve_card(broker_card()).await;
        let tool = ListAgentsTool::new(vec![]).with_gateway_port(port);

        let output = list(&tool).await;
        assert_eq!(output.total, 3);
        assert_eq!(find(&output, "local-agent-0").origin, AgentOrigin::Local);
        assert_eq!(
            find(&output, "local-agent-0").skills,
            vec!["delegate".to_string()]
        );
        assert_eq!(find(&output, "leased-vm").origin, AgentOrigin::Fleet);
        assert!(find(&output, "leased-vm").origin.is_own_fleet());

        // A card with no origin is not treated as trusted.
        assert_eq!(find(&output, "mystery").origin, AgentOrigin::Discovered);
        assert!(!find(&output, "mystery").origin.is_own_fleet());
    }

    /// ADR-0011 C10: the broker's card says what a worker runs, so for a
    /// local worker the live description beats the static stand-in text.
    #[tokio::test]
    async fn a_local_workers_live_card_beats_the_static_stand_in() {
        let card = serde_json::json!({
            "agents": [
                {"name": "local-coder", "origin": "local",
                 "description": "Model: qwen. Tools: the full set."},
                {"name": "local-reviewer", "origin": "local",
                 "description": "Model: gemma. Tool groups disabled: fs-write, shell, git."},
            ],
        });
        let (port, _server) = serve_card(card).await;
        let tool = ListAgentsTool::new(vec![])
            .with_local_workers(["local-coder", "local-reviewer"])
            .with_gateway_port(port);

        let output = list(&tool).await;
        assert_eq!(output.total, 2);
        assert_eq!(
            find(&output, "local-coder").description,
            "Model: qwen. Tools: the full set."
        );
        assert_eq!(
            find(&output, "local-reviewer").description,
            "Model: gemma. Tool groups disabled: fs-write, shell, git."
        );
        assert_eq!(find(&output, "local-reviewer").kind, "worker");
    }

    /// Without a gateway answering, the declared names are still listed —
    /// the stand-in text, so the model knows they exist.
    #[tokio::test]
    async fn declared_local_workers_are_listed_even_when_the_broker_is_silent() {
        let tool =
            ListAgentsTool::new(vec![]).with_local_workers(["local-coder", "local-reviewer"]);
        let output = list(&tool).await;
        assert_eq!(output.total, 2);
        assert_eq!(find(&output, "local-coder").origin, AgentOrigin::Local);
        assert_eq!(find(&output, "local-reviewer").origin, AgentOrigin::Local);
    }

    /// The user's own label wins over the broker's for an agent that is in
    /// both: what the user configured is the more informative answer.
    #[tokio::test]
    async fn a_configured_agent_keeps_its_label_when_the_broker_also_serves_it() {
        let card = serde_json::json!({
            "agents": [{"name": "voucher-agent", "origin": "local"}],
        });
        let (port, _server) = serve_card(card).await;
        let tool = ListAgentsTool::new(vec![make_agent(
            "voucher-agent",
            "https://hive.dev/a2a/voucher",
            true,
        )])
        .with_gateway_port(port);

        let output = list(&tool).await;
        assert_eq!(output.total, 1);
        assert_eq!(
            find(&output, "voucher-agent").origin,
            AgentOrigin::RemoteConfigured
        );
    }

    /// No gateway is not an error: the tool answers with what settings know.
    #[tokio::test]
    async fn a_gateway_that_is_not_there_leaves_the_settings_listing_alone() {
        let port = {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            listener.local_addr().unwrap().port()
        };
        let tool = ListAgentsTool::new(vec![make_agent("remote", "https://example.com/a2a", true)])
            .with_gateway_port(port);

        let output = list(&tool).await;
        assert_eq!(output.total, 1);
        assert_eq!(
            find(&output, "remote").origin,
            AgentOrigin::RemoteConfigured
        );
    }

    /// Serve one fixed card on a loopback port, as the gateway would.
    async fn serve_card(card: serde_json::Value) -> (u16, tokio::task::JoinHandle<()>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a loopback port");
        let port = listener.local_addr().expect("the bound address").port();
        let body = card.to_string();

        let server = tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let mut buffer = [0u8; 1024];
                let _ = socket.read(&mut buffer).await;
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.flush().await;
            }
        });

        (port, server)
    }
}
