//! What the model can address, and whose machine each one runs on
//! (ADR-0011 C5).
//!
//! The list is the roster's ([`crate::services::roster`], PL-S1 RO-1): remote
//! A2A agents the user configured, the agent specs the broker serves (PL-U5:
//! one kind of local agent, a spec run by the harness; a WASM plugin is a
//! tool inside one, never an agent), the nodes running in the broker's
//! directory, and hosted agents. A worker that registered a minute ago is
//! addressable and so belongs here, and only the broker knows it exists.
//! A name two origins share is listed under both, the lower one marked
//! `shadowed_by` the origin that owns the name.
//!
//! Every entry carries an [`AgentOrigin`], because "voucher-agent" and
//! "local-agent" read the same to a model and one of them is somebody else's
//! server.

use std::path::PathBuf;
use std::sync::Arc;

use rig_agent::tool::{Tool, ToolContext, ToolExecutionError};
use serde::{Deserialize, Serialize};

pub use crate::services::roster::PluginGrants;
use crate::services::lazy_broker::LazyBroker;
use crate::services::roster::{
    CallerView, CardFetch, DirectorySource, FetchError, HostedSource, LazyBrokerCards, LiveCards,
    LocalSpecSource, Origin, RemoteSource, Roster, RosterEntry, RosterSource, TransportCards,
};
use crate::settings::models::a2a_store::A2aAgentConfig;
use crate::tools::ToolError;
use chatty_fabric::{AgentOrigin, ConversationScope, Transport};

/// The conversation the broker files the root's nodes under (the
/// gateway's `ROOT_SCOPE`).
const ROOT_SCOPE: &str = "root";

/// Arguments for listing A2A agents (no arguments needed)
#[derive(Deserialize, Serialize)]
pub struct ListAgentsToolArgs {}

/// One agent the model can address, with where it runs.
///
/// The shape the model sees is flat and uniform on purpose: the interesting
/// question about an agent is not which of five sources it came from, it is
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
    /// Set when another agent of a higher origin has the same name: a call
    /// by this name reaches that one, not this.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shadowed_by: Option<Origin>,
}

impl From<RosterEntry> for AgentListing {
    fn from(entry: RosterEntry) -> Self {
        Self {
            name: entry.name,
            origin: entry.card.agent_origin,
            kind: match entry.origin {
                Origin::Remote => "remote",
                Origin::Hosted | Origin::LocalSpec | Origin::Node | Origin::Handle => "worker",
            },
            description: entry.card.description,
            url: entry.card.url,
            enabled: entry.card.enabled,
            skills: entry.card.skills,
            has_api_key: entry.card.has_api_key,
            plugins: entry.card.plugins,
            shadowed_by: entry.shadowed_by,
        }
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

/// Tool that lists all available agents: the roster.
///
/// This gives the LLM visibility into what agents are available, including their
/// names, URLs/types, and skills. Each agent is invokable via `invoke_agent`
/// and the `/agent <name> <prompt>` command.
#[derive(Clone)]
pub struct ListAgentsTool {
    /// Snapshot of configured remote A2A agents taken at construction time.
    remote_agents: Vec<A2aAgentConfig>,
    /// The specs the broker serves (ADR-0011 C2, named under C10), if it
    /// exists.
    local_workers: Vec<String>,
    /// A broker that has not necessarily started yet (BI-2, AGE-634).
    lazy_broker: Option<Arc<dyn LazyBroker>>,
    /// The fabric the broker's directory is read through (ADR-0020, BI-4):
    /// a worker's broker-made connection.
    transport: Option<Arc<dyn Transport>>,
    /// The broker's cards, cached for [`LiveCards::TTL`] across calls.
    cards: Option<Arc<LiveCards>>,
    /// Where a local agent's spec is looked up for its card: the workspace
    /// and the data directory (then the presets).
    spec_workspace: Option<PathBuf>,
    spec_data_dir: Option<PathBuf>,
}

impl ListAgentsTool {
    pub fn new(remote_agents: Vec<A2aAgentConfig>) -> Self {
        Self {
            remote_agents,
            local_workers: Vec::new(),
            lazy_broker: None,
            transport: None,
            cards: None,
            spec_workspace: None,
            spec_data_dir: dirs::data_dir(),
        }
    }

    /// Start the broker itself on first use instead of expecting it already
    /// running (BI-2, AGE-634), and read its live directory as well as
    /// settings (ADR-0011 C5).
    pub fn with_lazy_broker(mut self, broker: Arc<dyn LazyBroker>) -> Self {
        self.lazy_broker = Some(broker);
        self.reset_cards();
        self
    }

    /// Read the broker's directory over `transport` (ADR-0020, BI-4).
    pub fn with_transport(mut self, transport: Arc<dyn Transport>) -> Self {
        self.transport = Some(transport);
        self.reset_cards();
        self
    }

    /// Look local agents' specs up under `workspace` and `data_dir` (then
    /// the presets). Without it: the platform data directory, then the
    /// presets.
    pub fn with_spec_dirs(mut self, workspace: Option<PathBuf>, data_dir: Option<PathBuf>) -> Self {
        self.spec_workspace = workspace;
        self.spec_data_dir = data_dir;
        self
    }

    /// The specs the broker serves: chatty agents in their own process
    /// (ADR-0011 C2), one per name (C10). The broker's card, when it
    /// answers, says what each one runs — its model and tool set.
    pub fn with_local_workers<I, S>(mut self, names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.local_workers = names.into_iter().map(Into::into).collect();
        self
    }

    /// The broker's directory is read through the transport this tool was
    /// given, else the lazy broker's direct handle (starting it).
    fn reset_cards(&mut self) {
        let fetch: Option<Arc<dyn CardFetch>> = match (&self.transport, &self.lazy_broker) {
            (Some(transport), _) => Some(Arc::new(TransportCards(transport.clone()))),
            (None, Some(broker)) => Some(Arc::new(LazyBrokerCards(broker.clone()))),
            (None, None) => None,
        };
        self.cards = fetch.map(|fetch| Arc::new(LiveCards::new(fetch)));
    }

    fn roster(&self) -> Roster {
        let local = LocalSpecSource::load(
            &self.local_workers,
            self.spec_workspace.as_deref(),
            self.spec_data_dir.as_deref(),
            self.cards.clone(),
        );
        let mut sources: Vec<Arc<dyn RosterSource>> = vec![
            Arc::new(RemoteSource::new(self.remote_agents.clone())),
            Arc::new(HostedSource),
        ];
        if let Some(cards) = &self.cards {
            sources.push(Arc::new(DirectorySource::new(
                cards.clone(),
                self.local_workers.clone(),
            )));
        }
        sources.push(Arc::new(local));
        Roster::new(sources)
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
        // The broker scopes its own directory; this tool's caller is the
        // root's conversation as the broker knows it (RO-3 threads the
        // conversation through).
        let scope = ConversationScope::new(ROOT_SCOPE);
        let entries = self.roster().for_caller(&CallerView { scope: &scope }).await;
        let agents: Vec<AgentListing> = entries.into_iter().map(AgentListing::from).collect();
        tracing::info!(
            agent_count = agents.len(),
            live_read = self.cards.is_some(),
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
             called. An entry with `shadowed_by` shares its name with a higher-precedence agent, \
             which is the one a call by that name reaches."
                .to_string()
        };
        // AGE-746: a broker that is configured but failed to start (its
        // gateway socket in use, most often) used to be indistinguishable from one that
        // was simply never turned on. Say so, so the model can tell the user
        // rather than reporting local agents as reachable. The read is the
        // roster's own, cached: no second fetch.
        if let Some(cards) = &self.cards
            && let Err(FetchError::BrokerDidNotStart(error)) = cards.get().await
        {
            note = format!(
                "The local broker failed to start ({error}), so live agent data (and any agent \
                 only the broker knows about) is unavailable; entries below may be stale or \
                 unreachable. This is often another chatty process already serving the gateway \
                 socket — check Settings → Agents, or close the other process. {note}"
            );
        }

        Ok(ListAgentsToolOutput {
            total: agents.len(),
            agents,
            note,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::lazy_broker::LazyBroker;
    use crate::settings::models::a2a_store::A2aAgentConfig;
    use chatty_fabric::wire::{AgentEntry, ParticipantCard, ParticipantSkill};
    use chatty_fabric::{CallEvent, CallRequest, CallResult};

    /// A broker that is configured but never manages to bind its socket —
    /// standing in for the AGE-746 collision case.
    struct FailingBroker;

    #[async_trait::async_trait]
    impl LazyBroker for FailingBroker {
        async fn transport(&self) -> anyhow::Result<Option<Arc<dyn Transport>>> {
            Err(anyhow::anyhow!(
                "failed to serve the gateway: Address already in use (os error 98)"
            ))
        }

        fn bound_sockets(&self) -> Vec<PathBuf> {
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
            allow_private_network: false,
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
    /// failure reaches the note the model reads.
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
        assert_eq!(find(&output, "local-agent").kind, "worker");
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

    /// A directory entry the broker would list.
    fn entry(name: &str, description: &str, origin: AgentOrigin, skills: &[&str]) -> AgentEntry {
        AgentEntry::from_card(
            &ParticipantCard {
                name: name.to_string(),
                description: description.to_string(),
                skills: skills
                    .iter()
                    .map(|skill| ParticipantSkill {
                        name: skill.to_string(),
                        ..ParticipantSkill::default()
                    })
                    .collect(),
                ..ParticipantCard::default()
            },
            origin,
        )
    }

    /// The directory the broker would serve, with the origins it would put
    /// on it.
    fn broker_card() -> Vec<AgentEntry> {
        vec![
            entry(
                "local-agent-0",
                "A chatty agent in its own process",
                AgentOrigin::Local,
                &["delegate"],
            ),
            entry(
                "leased-vm",
                "A worker in a leased microVM",
                AgentOrigin::Fleet,
                &[],
            ),
            entry("", "No name: not addressable", AgentOrigin::Local, &[]),
        ]
    }

    /// The issue's "Verify", live half: a worker that registered after the
    /// tool was built is listed, as local, without anyone reconfiguring
    /// anything.
    #[tokio::test]
    async fn the_broker_s_live_participants_are_listed_with_their_origin() {
        let directory = directory(broker_card());
        let tool = ListAgentsTool::new(vec![]).with_transport(directory);

        let output = list(&tool).await;
        assert_eq!(output.total, 2, "an entry with no name is skipped");
        assert_eq!(find(&output, "local-agent-0").origin, AgentOrigin::Local);
        assert_eq!(
            find(&output, "local-agent-0").skills,
            vec!["delegate".to_string()]
        );
        assert_eq!(find(&output, "leased-vm").origin, AgentOrigin::Fleet);
        assert!(find(&output, "leased-vm").origin.is_own_fleet());
    }

    /// ADR-0011 C10: the broker's card says what a worker runs, so for a
    /// local worker the live description beats the static stand-in text.
    #[tokio::test]
    async fn a_local_workers_live_card_beats_the_static_stand_in() {
        let card = vec![
            entry(
                "local-coder",
                "Model: qwen. Tools: the full set.",
                AgentOrigin::Local,
                &[],
            ),
            entry(
                "local-reviewer",
                "Model: gemma. Tool groups disabled: fs-write, shell, git.",
                AgentOrigin::Local,
                &[],
            ),
        ];
        let directory = directory(card);
        let tool = ListAgentsTool::new(vec![])
            .with_local_workers(["local-coder", "local-reviewer"])
            .with_transport(directory);

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

    /// PL-D4's precedence: a running node owns its name over a configured
    /// remote of the same name, and the remote stays listed, shadowed.
    #[tokio::test]
    async fn a_node_shadows_a_configured_remote_of_the_same_name() {
        let card = vec![entry("voucher-agent", "a node", AgentOrigin::Local, &[])];
        let directory = directory(card);
        let tool = ListAgentsTool::new(vec![make_agent(
            "voucher-agent",
            "https://hive.dev/a2a/voucher",
            true,
        )])
        .with_transport(directory);

        let output = list(&tool).await;
        assert_eq!(output.total, 2);
        assert_eq!(output.agents[0].origin, AgentOrigin::Local);
        assert_eq!(output.agents[0].shadowed_by, None);
        assert_eq!(output.agents[1].origin, AgentOrigin::RemoteConfigured);
        assert_eq!(output.agents[1].shadowed_by, Some(Origin::Node));
        let json = serde_json::to_value(&output.agents).unwrap();
        assert!(json[0].get("shadowed_by").is_none(), "{json}");
        assert_eq!(json[1]["shadowed_by"], "node");
    }

    /// The tool's schema and description are constants (C10 rule; RO-3
    /// owns the byte-stability test across roster sizes).
    #[test]
    fn the_tool_definition_does_not_depend_on_the_roster() {
        let empty = ListAgentsTool::new(vec![]);
        let full = ListAgentsTool::new(vec![make_agent("a", "https://example.com/a", true)])
            .with_local_workers(["local-coder", "local-reviewer"])
            .with_transport(directory(broker_card()));
        assert_eq!(empty.description(), full.description());
        assert_eq!(empty.parameters(), full.parameters());
    }

    /// A broker that does not answer is not an error: the tool answers with
    /// what settings know.
    #[tokio::test]
    async fn a_broker_that_does_not_answer_leaves_the_settings_listing_alone() {
        let tool = ListAgentsTool::new(vec![make_agent("remote", "https://example.com/a2a", true)])
            .with_transport(Arc::new(Silent));

        let output = list(&tool).await;
        assert_eq!(output.total, 1);
        assert_eq!(
            find(&output, "remote").origin,
            AgentOrigin::RemoteConfigured
        );
    }

    /// A broker whose directory is `card`'s agents, as the gateway's
    /// aggregated card lists them.
    fn directory(agents: Vec<AgentEntry>) -> Arc<dyn Transport> {
        Arc::new(Directory(agents))
    }

    struct Directory(Vec<AgentEntry>);

    #[async_trait::async_trait]
    impl Transport for Directory {
        async fn call(
            &self,
            _req: CallRequest,
        ) -> Result<chatty_fabric::CallStream, chatty_fabric::CallError> {
            use futures::StreamExt;
            Ok(
                futures::stream::iter([Ok(CallEvent::Result(CallResult::Agents(self.0.clone())))])
                    .boxed(),
            )
        }
    }

    /// A broker that refuses every call.
    struct Silent;

    #[async_trait::async_trait]
    impl Transport for Silent {
        async fn call(
            &self,
            _req: CallRequest,
        ) -> Result<chatty_fabric::CallStream, chatty_fabric::CallError> {
            Err(chatty_fabric::CallError::Disconnected("gone".into()))
        }
    }
}
