//! What the model can address, and whose machine each one runs on
//! (ADR-0011 C5).
//!
//! Two sources, one list. The static half is settings: remote A2A agents the
//! user configured and WASM modules installed on disk. The live half is the
//! broker's own aggregated card, read over HTTP — a worker that registered a
//! minute ago is addressable and so belongs here, and only the broker knows
//! it exists.
//!
//! Every entry carries an [`AgentOrigin`], because "voucher-agent" and
//! "local-agent" read the same to a model and one of them is somebody else's
//! server.

use std::collections::BTreeMap;
use std::time::Duration;

use rig_agent::tool::{Tool, ToolContext, ToolExecutionError};
use serde::{Deserialize, Serialize};

use crate::settings::models::a2a_store::A2aAgentConfig;
use crate::tools::ToolError;
use crate::tools::agent_origin::AgentOrigin;

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

/// Summary of a locally installed WASM module agent, safe for display to the LLM.
#[derive(Debug, Serialize, Clone)]
pub struct LocalModuleAgentSummary {
    pub name: String,
    pub version: String,
    pub description: String,
    /// Tools exposed by the module.
    pub tools: Vec<String>,
    /// Whether the module supports the A2A protocol (accessible via the protocol gateway).
    pub supports_a2a: bool,
    /// Module execution mode from the manifest (`local`, `remote`, `remote_only`).
    #[serde(default)]
    pub execution_mode: String,
}

/// The broker's local-worker agent, when the gateway publishes one.
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
    /// `remote`, `module`, `worker` — how to think about what it is, not
    /// where it is.
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
}

/// Output from the list_agents tool
#[derive(Debug, Serialize)]
pub struct ListAgentsToolOutput {
    /// Every addressable agent, in name order, each carrying its origin.
    pub agents: Vec<AgentListing>,
    pub total: usize,
    pub note: String,
}

/// Tool that lists all available agents: both remotely configured A2A agents and
/// locally installed WASM module agents.
///
/// This gives the LLM visibility into what agents are available, including their
/// names, URLs/types, and skills/tools. Each agent is invokable via the
/// `/agent <name> <prompt>` command.
#[derive(Clone)]
pub struct ListAgentsTool {
    /// Snapshot of configured remote A2A agents taken at construction time.
    remote_agents: Vec<A2aAgentConfig>,
    /// Locally installed WASM module agents with `agent = true`.
    module_agents: Vec<LocalModuleAgentSummary>,
    /// The broker's local worker (ADR-0011 C2), if the gateway is running.
    local_worker: Option<LocalWorkerAgentSummary>,
    /// Where to read the broker's live participant table, when the gateway is
    /// running (ADR-0011 C5).
    gateway_base_url: Option<String>,
    http: reqwest::Client,
}

impl ListAgentsTool {
    pub fn new(remote_agents: Vec<A2aAgentConfig>) -> Self {
        Self {
            remote_agents,
            module_agents: Vec::new(),
            local_worker: None,
            gateway_base_url: None,
            http: reqwest::Client::new(),
        }
    }

    /// Create a new `ListAgentsTool` that also reports local WASM module agents.
    pub fn new_with_modules(
        remote_agents: Vec<A2aAgentConfig>,
        module_agents: Vec<LocalModuleAgentSummary>,
    ) -> Self {
        Self {
            remote_agents,
            module_agents,
            local_worker: None,
            gateway_base_url: None,
            http: reqwest::Client::new(),
        }
    }

    /// Read the broker's live participant table as well as settings
    /// (ADR-0011 C5). Without it, this lists only what was configured.
    pub fn with_gateway_port(mut self, port: u16) -> Self {
        self.gateway_base_url = Some(format!("http://localhost:{port}"));
        self
    }

    /// Advertise the broker's local worker: a chatty agent in its own
    /// process (ADR-0011 C2).
    pub fn with_local_worker(mut self, name: impl Into<String>) -> Self {
        self.local_worker = Some(LocalWorkerAgentSummary {
            name: name.into(),
            description: "A chatty agent in its own process and its own \
                          workspace, with the same tool set. Delegate a \
                          self-contained task to it."
                .to_string(),
        });
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
                },
            );
        }

        for module in &self.module_agents {
            listings.insert(
                module.name.clone(),
                AgentListing {
                    name: module.name.clone(),
                    origin: AgentOrigin::Local,
                    kind: "module",
                    description: module.description.clone(),
                    url: None,
                    enabled: true,
                    skills: module.tools.clone(),
                    has_api_key: false,
                },
            );
        }

        if let Some(worker) = self.local_worker.as_ref() {
            listings.insert(
                worker.name.clone(),
                AgentListing {
                    name: worker.name.clone(),
                    origin: AgentOrigin::Local,
                    kind: "worker",
                    description: worker.description.clone(),
                    url: None,
                    enabled: true,
                    skills: Vec::new(),
                    has_api_key: false,
                },
            );
        }

        // The live half. A remote agent the user configured keeps its own
        // entry: the broker would report it as whatever it is to the broker,
        // and what the *user* did is the more informative label.
        for live in self.live_agents().await {
            listings.entry(live.name.clone()).or_insert(live);
        }

        let agents: Vec<AgentListing> = listings.into_values().collect();
        tracing::info!(
            agent_count = agents.len(),
            live_read = self.gateway_base_url.is_some(),
            "list_agents called"
        );

        let note = if agents.is_empty() {
            "No agents are available. Remote agents can be added via Settings → A2A Agents. \
             Local WASM module agents are installed in the modules directory."
                .to_string()
        } else {
            "To invoke an agent, use the `invoke_agent` tool with the agent's name and a prompt. \
             `origin` says whose machine it runs on: `local` is this machine, `fleet` is a \
             machine this user leased, `remote_configured` is a third-party URL the user \
             configured, `discovered` is a third party nobody chose. Only enabled agents can be \
             called."
                .to_string()
        };

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

    /// The broker's live participant table, from its aggregated agent card.
    ///
    /// Failure is not an error: the gateway may be off, and this tool's job is
    /// to say what can be addressed, which is then nothing but settings.
    async fn live_agents(&self) -> Vec<AgentListing> {
        let Some(base) = self.gateway_base_url.as_ref() else {
            return Vec::new();
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
                        return Vec::new();
                    }
                }
            }
            Ok(response) => {
                tracing::debug!(%url, status = %response.status(), "the broker refused its card");
                return Vec::new();
            }
            Err(error) => {
                tracing::debug!(%url, ?error, "no broker to list live agents from");
                return Vec::new();
            }
        };

        card.get("agents")
            .and_then(|agents| agents.as_array())
            .map(|agents| agents.iter().filter_map(listing_from_card).collect())
            .unwrap_or_default()
    }
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
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::models::a2a_store::A2aAgentConfig;

    fn make_agent(name: &str, url: &str, enabled: bool) -> A2aAgentConfig {
        A2aAgentConfig {
            name: name.to_string(),
            url: url.to_string(),
            api_key: None,
            enabled,
            skills: vec![],
        }
    }

    fn make_module_agent(name: &str) -> LocalModuleAgentSummary {
        LocalModuleAgentSummary {
            name: name.to_string(),
            version: "0.1.0".to_string(),
            description: format!("{name} module agent"),
            tools: vec!["tool_a".to_string()],
            supports_a2a: true,
            execution_mode: "local".to_string(),
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

    /// Modules and the broker's worker are this machine's.
    #[tokio::test]
    async fn a_module_and_the_local_worker_are_local() {
        let tool = ListAgentsTool::new_with_modules(
            vec![make_agent("remote", "https://example.com/a2a", true)],
            vec![make_module_agent("echo")],
        )
        .with_local_worker("local-agent");

        let output = list(&tool).await;
        assert_eq!(output.total, 3);
        assert_eq!(find(&output, "echo").origin, AgentOrigin::Local);
        assert_eq!(find(&output, "echo").kind, "module");
        assert_eq!(find(&output, "local-agent").origin, AgentOrigin::Local);
        assert_eq!(find(&output, "local-agent").kind, "worker");
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
