//! What the model can address: the roster (PL-S1 RO-3, vault
//! `fabric-roster.md` §3.3), filtered by the caller's delegation policy and
//! scoped to its conversation.
//!
//! The tool reads [`Roster::for_caller`] over five sources — resumable
//! handles, running nodes, the agent specs this machine can start, hosted
//! agents and the remote A2A agents in settings — and offers only the
//! entries DP-1's [`may_call`] lets this caller reach. A plain root (no
//! spec) may call anyone.
//!
//! Nodes and handles come from the caller's [`ConversationScope`] only.
//! Where the host holds the broker's directory itself it hands the tool a
//! [`NodeTable`] and [`DirectorySource`] reads `in_scope` from it. Otherwise
//! (the in-process root over its direct handle, a worker over the fabric)
//! the nodes are the ones the broker's own `ListAgents` answer names
//! ([`BrokerNodeSource`]): the broker is the one that knows which
//! conversation the calling node works for, so it answers in scope. Today
//! it admits every node into its one scope.
//!
//! The tool's name, description and schema are constants: the roster only
//! ever appears in a call's *result*, never in the tool block or the
//! preamble (ADR-0011 C10), so the prompt prefix stays byte-identical
//! whatever the team.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use rig_agent::tool::{Tool, ToolContext, ToolExecutionError};
use serde::{Deserialize, Serialize};

use crate::agent_spec::{AgentSpec, load_agent_spec_from};
use crate::services::delegation_policy::{self, may_call};
use crate::services::lazy_broker::LazyBroker;
use crate::services::roster::{
    self, BrokerNodeSource, CardFetch, DirectorySource, HandleIndex, HandleSource, HandleState,
    HostedSource, LazyBrokerCards, LiveCards, LocalSpecSource, NodeTable, Origin, Reach,
    RemoteSource, Roster, RosterEntry, RosterSource, TransportCards,
};
use crate::settings::models::a2a_store::A2aAgentConfig;
use crate::tools::ToolError;
use chatty_fabric::{AgentOrigin, ConversationScope, Transport};

/// The scope the broker admits every node into until it admits them per
/// conversation: the gateway's `ROOT_SCOPE`.
const BROKER_SCOPE: &str = "root";

/// The tool's description: a constant, whatever the roster (ADR-0011 C10).
const DESCRIPTION: &str = "List the agents you may delegate to: agent specs this machine can \
     start, agents already running in this conversation's swarm, and remote A2A agents \
     configured in Settings. Returns each agent's name, origin, reach, a one-line description \
     and, when known, its model, tool profile and plugins. Only agents you are allowed to call \
     are listed.\n\nUse this to discover what agents are available before deciding whether to \
     delegate a task. Call one with `invoke_agent` — each agent runs its own full agentic loop \
     and returns a result.";

/// Arguments for listing agents (none).
#[derive(Deserialize, Serialize)]
pub struct ListAgentsToolArgs {}

/// One agent the model can address: a compact row of the roster.
#[derive(Debug, Serialize, Clone, PartialEq)]
pub struct AgentListing {
    pub name: String,
    /// Which roster source it comes from: `handle`, `node`, `local_spec`,
    /// `hosted` or `remote`.
    pub origin: Origin,
    /// `live`, `startable`, `idle`, or `unreachable` with its `reason`.
    #[serde(flatten)]
    pub reach: Reach,
    /// One line.
    pub description: String,
    /// Whose machine it runs on (ADR-0011 C5): `local` is this machine,
    /// `remote_configured` a third-party URL the user configured.
    pub runs_on: AgentOrigin,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Its tool profile.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// Its plugins, by module name.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub plugins: Vec<String>,
    /// Set when another entry owns this name: a call by it reaches that one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shadowed_by: Option<Origin>,
    /// A handle's row (RO-4 fills it in).
    #[serde(flatten)]
    pub handle: Option<HandleRow>,
}

/// What a handle's listing adds.
#[derive(Debug, Serialize, Clone, PartialEq)]
pub struct HandleRow {
    pub owner: String,
    pub state: HandleState,
    pub pending_messages: u32,
    pub behind_by: Option<u32>,
}

/// Output from the list_agents tool
#[derive(Debug, Serialize)]
pub struct ListAgentsToolOutput {
    /// Every agent this caller may call, in name order.
    pub agents: Vec<AgentListing>,
    pub total: usize,
    pub note: String,
}

/// Tool that lists the agents this caller may delegate to.
#[derive(Clone)]
pub struct ListAgentsTool {
    /// Snapshot of configured remote A2A agents taken at construction time.
    remote_agents: Vec<A2aAgentConfig>,
    /// The agent specs this machine can start (ADR-0011 C2, named under
    /// C10; PL-U5), by name.
    local_workers: Vec<String>,
    /// A broker that has not necessarily started yet (BI-2, AGE-634).
    lazy_broker: Option<Arc<dyn LazyBroker>>,
    /// The fabric the broker's directory is read through (ADR-0020, BI-4):
    /// a worker's broker-made connection. Preferred over `lazy_broker`.
    transport: Option<Arc<dyn Transport>>,
    /// The broker's cards, cached for [`LiveCards::TTL`] across calls.
    cards: Option<Arc<LiveCards>>,
    /// Where a local agent's spec is looked up: the workspace and the data
    /// directory (then the presets).
    spec_workspace: Option<PathBuf>,
    spec_data_dir: Option<PathBuf>,
    /// The spec this agent runs as; `None` is a plain root, which may call
    /// anyone.
    caller: Option<AgentSpec>,
    /// The caller's root conversation.
    scope: ConversationScope,
    /// The broker's directory, when the host holds it.
    nodes: Option<Arc<dyn NodeTable>>,
    /// Resumable handles (RC-3).
    handles: Option<Arc<dyn HandleIndex>>,
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
            caller: None,
            scope: ConversationScope::new(BROKER_SCOPE),
            nodes: None,
            handles: None,
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

    /// The agent specs this machine can start, by name. The broker's live
    /// card, when it answers, supplies each one's description — its model
    /// and tool set; the spec's own text stands in when it does not.
    pub fn with_local_workers<I, S>(mut self, names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.local_workers = names.into_iter().map(Into::into).collect();
        self
    }

    /// Offer only what `spec`'s delegation policy lets it call (DP-1).
    /// `None` is a plain root, which may call anyone.
    pub fn with_caller(mut self, spec: Option<AgentSpec>) -> Self {
        self.caller = spec;
        self
    }

    /// The caller's root conversation: nodes and handles of any other are
    /// never listed.
    pub fn with_scope(mut self, scope: ConversationScope) -> Self {
        self.scope = scope;
        self
    }

    /// Read nodes from the broker's directory directly, by scope, instead of
    /// from the broker's answer.
    pub fn with_node_table(mut self, nodes: Arc<dyn NodeTable>) -> Self {
        self.nodes = Some(nodes);
        self
    }

    /// List the resumable handles `index` holds for the caller's scope.
    pub fn with_handles(mut self, index: Arc<dyn HandleIndex>) -> Self {
        self.handles = Some(index);
        self
    }

    /// The cards come over the transport when there is one, else from the
    /// lazy broker (starting it).
    fn reset_cards(&mut self) {
        let fetch: Arc<dyn CardFetch> = match (&self.transport, &self.lazy_broker) {
            (Some(transport), _) => Arc::new(TransportCards(transport.clone())),
            (None, Some(broker)) => Arc::new(LazyBrokerCards(broker.clone())),
            (None, None) => return,
        };
        self.cards = Some(Arc::new(LiveCards::new(fetch)));
    }

    /// The spec behind each name this machine can start: the one on disk,
    /// else a bare spec (lists nobody, exposed to anyone), as the broker's
    /// policy reads a name it has no spec for.
    fn local_specs(&self) -> BTreeMap<String, AgentSpec> {
        self.local_workers
            .iter()
            .map(|name| {
                let spec = load_agent_spec_from(
                    name,
                    self.spec_workspace.as_deref(),
                    self.spec_data_dir.as_deref(),
                )
                .map(|loaded| loaded.spec)
                .unwrap_or_else(|_| AgentSpec::named(name));
                (name.clone(), spec)
            })
            .collect()
    }

    fn roster(&self, specs: &BTreeMap<String, AgentSpec>) -> Roster {
        let mut sources: Vec<Arc<dyn RosterSource>> = Vec::new();
        sources.push(Arc::new(match &self.handles {
            Some(index) => HandleSource::new(index.clone()),
            None => HandleSource::default(),
        }));
        match (&self.nodes, &self.cards) {
            (Some(table), Some(cards)) => {
                sources.push(Arc::new(DirectorySource::new(table.clone(), cards.clone())))
            }
            (None, Some(cards)) => sources.push(Arc::new(BrokerNodeSource::new(
                specs.keys().cloned(),
                cards.clone(),
            ))),
            (_, None) => {}
        }
        sources.push(Arc::new(LocalSpecSource::new(
            specs.values().cloned().collect(),
            self.cards.clone(),
        )));
        sources.push(Arc::new(HostedSource));
        sources.push(Arc::new(RemoteSource::new(self.remote_agents.clone())));
        Roster::new(sources)
    }

    /// The spec a call to `entry` is checked against: a local spec's or a
    /// node's own spec, else a bare one by name.
    fn callee_spec<'a>(
        entry: &RosterEntry,
        specs: &'a BTreeMap<String, AgentSpec>,
    ) -> std::borrow::Cow<'a, AgentSpec> {
        let name = match entry.origin {
            Origin::Node => node_spec(&entry.name).unwrap_or(&entry.name),
            _ => &entry.name,
        };
        let local = matches!(entry.origin, Origin::LocalSpec | Origin::Node);
        match specs.get(name).filter(|_| local) {
            Some(spec) => std::borrow::Cow::Borrowed(spec),
            None => std::borrow::Cow::Owned(AgentSpec::named(name)),
        }
    }

    /// Whether this caller may call `callee` (DP-1).
    fn may_reach(&self, callee: &AgentSpec) -> bool {
        self.caller.as_ref().is_none_or(|caller| {
            may_call(delegation_policy::CallerView { spec: caller }, callee).is_ok()
        })
    }
}

/// The caller `list_agents` checks against: the same spec the broker
/// checks the caller's own calls against. A root with a broker is checked
/// as the broker's root (`SpecPolicy::with_root`: `None` for a bare root,
/// which may call anyone, AGE-745); otherwise the spec this agent runs as
/// (a worker is admitted as its spec).
pub fn caller_policy(
    lazy_broker: Option<&Arc<dyn LazyBroker>>,
    own_spec: Option<AgentSpec>,
) -> Option<AgentSpec> {
    match lazy_broker {
        Some(broker) => broker.root_spec(),
        None => own_spec,
    }
}

/// Settles one collision away from PL-D4 (`handle > node > local_spec >
/// hosted > remote`): a configured remote owns a name only a running node
/// shares, because `invoke_agent` cannot address a node by name (routing to
/// one needs broker work that does not exist yet), so a call by that name
/// reaches the remote. A local spec with the name still outranks the
/// remote, as `invoke_agent` dispatches it (AGE-1131). A handle outranks it
/// here too, but `invoke_agent` cannot address handles yet: RO-4 must route
/// handle names before handles are wired in, or the two tools disagree again.
fn remote_over_uncallable_node(entries: Vec<RosterEntry>) -> Vec<RosterEntry> {
    let mut settled: Vec<RosterEntry> = Vec::with_capacity(entries.len());
    let mut group: Vec<RosterEntry> = Vec::new();
    let flush = |group: &mut Vec<RosterEntry>, settled: &mut Vec<RosterEntry>| {
        let has = |origin| group.iter().any(|entry| entry.origin == origin);
        if has(Origin::Remote)
            && has(Origin::Node)
            && !has(Origin::LocalSpec)
            && !has(Origin::Handle)
        {
            let at = group
                .iter()
                .position(|entry| entry.origin == Origin::Remote)
                .expect("a remote is in the group");
            let mut remote = group.remove(at);
            remote.shadowed_by = None;
            for other in group.iter_mut() {
                other.shadowed_by = Some(Origin::Remote);
            }
            settled.push(remote);
        }
        settled.append(group);
    };
    for entry in entries {
        if group.first().is_some_and(|first| first.name != entry.name) {
            flush(&mut group, &mut settled);
        }
        group.push(entry);
    }
    flush(&mut group, &mut settled);
    settled
}

/// The spec a node runs as: the broker names every node `<spec>-<n>`
/// (`chatty_fabric::Directory`).
fn node_spec(name: &str) -> Option<&str> {
    let (spec, n) = name.rsplit_once('-')?;
    (!spec.is_empty() && !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())).then_some(spec)
}

/// `entry` as the model reads it, with what its spec says.
fn listing(entry: RosterEntry, spec: &AgentSpec) -> AgentListing {
    let from_spec = matches!(entry.origin, Origin::LocalSpec | Origin::Node);
    AgentListing {
        name: entry.name,
        origin: entry.origin,
        reach: entry.reach,
        description: entry
            .card
            .description
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" "),
        runs_on: entry.card.agent_origin,
        model: from_spec.then(|| spec.agent.model.clone()).flatten(),
        profile: from_spec.then(|| spec.tools.profile.clone()).flatten(),
        plugins: if from_spec {
            spec.plugins
                .iter()
                .map(|plugin| plugin.module.clone())
                .collect()
        } else {
            Vec::new()
        },
        shadowed_by: entry.shadowed_by,
        handle: entry.handle.map(|info| HandleRow {
            owner: info.owner,
            state: info.state,
            pending_messages: info.pending_messages,
            behind_by: info.behind_by,
        }),
    }
}

impl Tool for ListAgentsTool {
    const NAME: &'static str = "list_agents";
    type Error = ToolError;
    type Args = ListAgentsToolArgs;
    type Output = ListAgentsToolOutput;

    fn description(&self) -> String {
        DESCRIPTION.to_string()
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
        let specs = self.local_specs();
        let entries = remote_over_uncallable_node(
            self.roster(&specs)
                .for_caller(&roster::CallerView { scope: &self.scope })
                .await,
        );
        let agents: Vec<AgentListing> = entries
            .into_iter()
            .filter_map(|entry| {
                let callee = Self::callee_spec(&entry, &specs).into_owned();
                self.may_reach(&callee).then(|| listing(entry, &callee))
            })
            .collect();
        // Cached: the sources above already fetched, so this reads the same
        // answer they did.
        let broker_error = match &self.cards {
            Some(cards) => cards.get().await.err(),
            None => None,
        };
        tracing::info!(
            agent_count = agents.len(),
            live_read = self.cards.is_some(),
            caller = self.caller.as_ref().map(|spec| spec.agent.name.as_str()),
            "list_agents called"
        );

        let mut note = if agents.is_empty() {
            "No agents are available to you. Remote agents can be added via Settings → \
             Extensions; local agents are agent specs in `.chatty/agents/<name>.toml`, served by \
             the broker (Settings → Agents). A spec's `swarm.delegates_to` decides whom it may \
             call."
                .to_string()
        } else {
            "To invoke an agent, use the `invoke_agent` tool with the agent's name and a prompt. \
             `reach` says whether a call can be served: `live` and `idle` are running, \
             `startable` is started by the call, `unreachable` fails and says why. `runs_on` \
             says whose machine it runs on: `local` is this machine, `fleet` is a machine this \
             user leased, `remote_configured` is a third-party URL the user configured, \
             `discovered` is a third party nobody chose. A running node \
             (origin `node`, named `<spec>-<n>`) cannot be called by its own name: call its spec \
             instead."
                .to_string()
        };
        // AGE-746: a broker that is configured but failed to start (its
        // gateway socket in use, most often) used to be indistinguishable
        // from one that was simply never turned on. Say so, so the model can
        // tell the user rather than reporting local agents as reachable.
        if let Some(error) = broker_error {
            note = format!(
                "The local broker failed to start or did not answer ({error}), so live agent data \
                 (and any agent only the broker knows about) is unavailable; entries below may be \
                 stale or unreachable. This is often another chatty process already serving the \
                 gateway socket — check Settings → Agents, or close the other process. {note}"
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
    use crate::services::roster::HandleInfo;
    use crate::settings::models::a2a_store::A2aAgentConfig;
    use chatty_fabric::wire::{AgentEntry, ParticipantCard};
    use chatty_fabric::{CallEvent, CallRequest, CallResult, Directory};

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

    fn names(output: &ListAgentsToolOutput) -> Vec<&str> {
        output
            .agents
            .iter()
            .map(|agent| agent.name.as_str())
            .collect()
    }

    /// A workspace whose `.chatty/agents/` holds `specs` (name, TOML).
    fn workspace_with(specs: &[(&str, &str)]) -> tempfile::TempDir {
        let workspace = tempfile::tempdir().unwrap();
        let agents = workspace
            .path()
            .join(crate::agent_spec::WORKSPACE_AGENTS_DIR);
        std::fs::create_dir_all(&agents).unwrap();
        for (name, toml) in specs {
            std::fs::write(agents.join(format!("{name}.toml")), toml).unwrap();
        }
        workspace
    }

    #[tokio::test]
    async fn nothing_configured_lists_nothing() {
        let output = list(&ListAgentsTool::new(vec![])).await;
        assert_eq!(output.total, 0);
        assert!(output.agents.is_empty());
        assert!(output.note.contains("No agents are available"));
    }

    /// AGE-746: a bind failure (e.g. two chatty processes on the same port)
    /// used to be silent. Now the failure reaches the note the model reads.
    #[tokio::test]
    async fn gateway_bind_failure_is_reported() {
        let tool = ListAgentsTool::new(vec![])
            .with_lazy_broker(Arc::new(FailingBroker))
            .with_local_workers(["coder"]);

        let output = list(&tool).await;

        // The local spec still lists (a broker that never answers is not a
        // reason to hide it), but the note says the live data behind it
        // could not be reached.
        assert_eq!(find(&output, "coder").reach, Reach::Startable);
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

    /// A configured remote is labelled as somebody else's, and its key is
    /// never in the output.
    #[tokio::test]
    async fn a_configured_remote_is_remote_configured_and_keeps_its_key() {
        let mut agent = make_agent("voucher-agent", "https://hive.dev/a2a/voucher", true);
        agent.api_key = Some("secret-value".to_string());

        let output = list(&ListAgentsTool::new(vec![agent])).await;
        let listed = find(&output, "voucher-agent");

        assert_eq!(listed.origin, Origin::Remote);
        assert_eq!(listed.runs_on, AgentOrigin::RemoteConfigured);
        assert_eq!(listed.reach, Reach::Live);
        assert!(listed.description.contains("https://hive.dev/a2a/voucher"));

        let json = serde_json::to_string(&output.agents).expect("the listing serializes");
        assert!(
            !json.contains("secret-value"),
            "an api key must never reach the model: {json}"
        );
    }

    #[tokio::test]
    async fn a_disabled_remote_is_listed_as_unreachable_rather_than_hidden() {
        let output = list(&ListAgentsTool::new(vec![make_agent(
            "off-agent",
            "https://example.com/a2a",
            false,
        )]))
        .await;
        assert!(matches!(
            find(&output, "off-agent").reach,
            Reach::Unreachable { .. }
        ));
    }

    /// §3.3: a local agent's row carries its model, profile and plugin
    /// names from its spec; an agent without a spec shows none.
    #[tokio::test]
    async fn a_local_agents_row_shows_its_model_profile_and_plugins() {
        let workspace = workspace_with(&[(
            "auditor",
            "[agent]\nname = \"auditor\"\nmodel = \"qwen3:4b\"\n\n\
             [tools]\nprofile = \"reviewer\"\n\n\
             [[plugins]]\nmodule = \"config-reader\"\ngrants = [\"config\"]\n\n\
             [[plugins]]\nmodule = \"echo\"\n",
        )]);
        let tool = ListAgentsTool::new(vec![])
            .with_local_workers(["auditor", "local-agent"])
            .with_spec_dirs(Some(workspace.path().to_path_buf()), None);

        let output = list(&tool).await;
        let auditor = find(&output, "auditor");
        assert_eq!(auditor.origin, Origin::LocalSpec);
        assert_eq!(auditor.reach, Reach::Startable);
        assert_eq!(auditor.model.as_deref(), Some("qwen3:4b"));
        assert_eq!(auditor.profile.as_deref(), Some("reviewer"));
        assert_eq!(auditor.plugins, ["config-reader", "echo"]);
        assert!(find(&output, "local-agent").plugins.is_empty());

        let json = serde_json::to_value(&output.agents).unwrap();
        assert_eq!(
            json[0],
            serde_json::json!({
                "name": "auditor",
                "origin": "local_spec",
                "reach": "startable",
                "description": auditor.description,
                "runs_on": "local",
                "model": "qwen3:4b",
                "profile": "reviewer",
                "plugins": ["config-reader", "echo"],
            })
        );
        assert!(json[1].get("plugins").is_none(), "{json}");
    }

    /// PL-U5: the roster's specs are this machine's local agents; nothing
    /// else is listed as local.
    #[tokio::test]
    async fn the_roster_s_specs_are_the_local_agents() {
        let roster = crate::agent_spec::roster_names_from(&[], None, None);
        let tool = ListAgentsTool::new(vec![make_agent("remote", "https://example.com/a2a", true)])
            .with_local_workers(roster.iter().cloned());

        let output = list(&tool).await;
        assert_eq!(output.total, roster.len() + 1);
        for name in &roster {
            assert_eq!(find(&output, name).origin, Origin::LocalSpec);
            assert_eq!(find(&output, name).runs_on, AgentOrigin::Local);
        }
        assert_eq!(find(&output, "remote").origin, Origin::Remote);
        assert!(
            output.note.contains("runs_on"),
            "the model is told what the label means"
        );
    }

    #[tokio::test]
    async fn agents_are_listed_in_name_order() {
        let tool = ListAgentsTool::new(vec![
            make_agent("zeta", "https://example.com/z", true),
            make_agent("alpha", "https://example.com/a", true),
        ]);
        assert_eq!(names(&list(&tool).await), ["alpha", "zeta"]);
    }

    // ── Policy ───────────────────────────────────────────────────────────────

    /// Invariant 3: a caller whose `delegates_to = ["*-reviewer"]` sees only
    /// reviewers, of every origin, and only those that let it call them
    /// (DP-1's `may_call`: listed, exposed, `callers`). A plain root sees
    /// them all.
    #[tokio::test]
    async fn list_agents_filtered_by_policy() {
        let workspace = workspace_with(&[
            (
                "hidden-reviewer",
                "[agent]\nname = \"hidden-reviewer\"\n\n[swarm]\nexposed = false\n",
            ),
            (
                "picky-reviewer",
                "[agent]\nname = \"picky-reviewer\"\n\n[swarm]\ncallers = [\"lead\"]\n",
            ),
        ]);
        let broker = directory(vec![
            entry("local-reviewer-0", "A running reviewer", AgentOrigin::Local),
            entry("local-coder-0", "A running coder", AgentOrigin::Local),
        ]);
        let tool = |caller: Option<AgentSpec>| {
            ListAgentsTool::new(vec![
                make_agent("voucher-agent", "https://example.com/v", true),
                make_agent("remote-reviewer", "https://example.com/r", true),
            ])
            .with_local_workers([
                "local-coder",
                "local-reviewer",
                "hidden-reviewer",
                "picky-reviewer",
            ])
            .with_spec_dirs(Some(workspace.path().to_path_buf()), None)
            .with_transport(broker.clone())
            .with_caller(caller)
        };

        let mut triage = AgentSpec::named("triage");
        triage.swarm.delegates_to = vec!["*-reviewer".to_string()];
        let output = list(&tool(Some(triage))).await;
        assert_eq!(
            names(&output),
            ["local-reviewer", "local-reviewer-0", "remote-reviewer"],
            "{:?}",
            output.agents
        );
        assert_eq!(find(&output, "local-reviewer-0").origin, Origin::Node);

        let everyone = list(&tool(None)).await;
        assert_eq!(
            names(&everyone),
            [
                "hidden-reviewer",
                "local-coder",
                "local-coder-0",
                "local-reviewer",
                "local-reviewer-0",
                "picky-reviewer",
                "remote-reviewer",
                "voucher-agent",
            ]
        );
    }

    // ── Scope ────────────────────────────────────────────────────────────────

    /// One conversation's resumable handles.
    struct Handles;

    impl HandleIndex for Handles {
        fn handles(&self, scope: &ConversationScope) -> Vec<(String, HandleInfo)> {
            vec![(
                format!("{scope}-handle"),
                HandleInfo {
                    owner: format!("{scope}-owner"),
                    state: HandleState::Parked,
                    pending_messages: 2,
                    behind_by: Some(1),
                    idle_for_s: 60,
                },
            )]
        }
    }

    /// Invariant 4: two conversations on one broker never see each other's
    /// nodes or handles, though the broker serves cards for both.
    #[tokio::test]
    async fn roster_is_scoped_per_conversation() {
        let mut nodes = Directory::new();
        let a = nodes
            .admit("local-coder", None, ConversationScope::new("conv-a"))
            .unwrap();
        let b = nodes
            .admit("local-coder", None, ConversationScope::new("conv-b"))
            .unwrap();
        let (a, b) = (a.name().to_string(), b.name().to_string());
        let table: Arc<dyn NodeTable> = Arc::new(std::sync::Mutex::new(nodes));
        let broker = directory(vec![
            entry(&a, "conversation a's coder", AgentOrigin::Local),
            entry(&b, "conversation b's coder", AgentOrigin::Local),
        ]);
        let tool = |scope: &str| {
            ListAgentsTool::new(vec![])
                .with_transport(broker.clone())
                .with_node_table(table.clone())
                .with_handles(Arc::new(Handles))
                .with_scope(ConversationScope::new(scope))
        };

        let in_a = list(&tool("conv-a")).await;
        let in_b = list(&tool("conv-b")).await;

        assert_eq!(names(&in_a), ["conv-a-handle", a.as_str()]);
        assert_eq!(names(&in_b), ["conv-b-handle", b.as_str()]);
        assert_eq!(find(&in_a, &a).origin, Origin::Node);
        assert_eq!(find(&in_a, &a).description, "conversation a's coder");

        // A handle's row carries its owner, state and backlog (§3.3).
        let json = serde_json::to_value(find(&in_a, "conv-a-handle")).unwrap();
        assert_eq!(json["origin"], "handle");
        assert_eq!(json["owner"], "conv-a-owner");
        assert_eq!(json["state"], "parked");
        assert_eq!(json["pending_messages"], 2);
        assert_eq!(json["behind_by"], 1);
    }

    // ── The live half ────────────────────────────────────────────────────────

    /// A directory entry the broker would list.
    fn entry(name: &str, description: &str, origin: AgentOrigin) -> AgentEntry {
        AgentEntry::from_card(
            &ParticipantCard {
                name: name.to_string(),
                description: description.to_string(),
                ..ParticipantCard::default()
            },
            origin,
        )
    }

    /// A worker that registered after the tool was built is listed as a
    /// node, with whose machine it runs on; an entry with no name is not.
    #[tokio::test]
    async fn the_broker_s_running_nodes_are_listed_with_where_they_run() {
        let directory = directory(vec![
            entry(
                "local-agent-0",
                "A chatty agent in its own process",
                AgentOrigin::Local,
            ),
            entry(
                "leased-vm-0",
                "A worker in a leased microVM",
                AgentOrigin::Fleet,
            ),
            entry("", "No name: not addressable", AgentOrigin::Local),
        ]);
        let tool = ListAgentsTool::new(vec![]).with_transport(directory);

        let output = list(&tool).await;
        assert_eq!(output.total, 2, "an entry with no name is skipped");
        assert_eq!(find(&output, "local-agent-0").origin, Origin::Node);
        assert_eq!(find(&output, "local-agent-0").reach, Reach::Live);
        assert_eq!(find(&output, "local-agent-0").runs_on, AgentOrigin::Local);
        assert_eq!(find(&output, "leased-vm-0").runs_on, AgentOrigin::Fleet);
    }

    /// ADR-0011 C10: the broker's card says what a spec runs, so its live
    /// description beats the static stand-in text, and a spec the broker
    /// serves is not also listed as a node.
    #[tokio::test]
    async fn a_local_workers_live_card_beats_the_static_stand_in() {
        let directory = directory(vec![
            entry(
                "local-coder",
                "Model: qwen. Tools: the full set.",
                AgentOrigin::Local,
            ),
            entry(
                "local-reviewer",
                "Model: gemma.\n  Tool groups disabled: fs-write, shell, git.",
                AgentOrigin::Local,
            ),
        ]);
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
            "Model: gemma. Tool groups disabled: fs-write, shell, git.",
            "one line"
        );
        assert_eq!(find(&output, "local-reviewer").origin, Origin::LocalSpec);
    }

    /// Without a broker answering, the declared names are still listed —
    /// the stand-in text, so the model knows they exist.
    #[tokio::test]
    async fn declared_local_workers_are_listed_even_when_the_broker_is_silent() {
        let tool = ListAgentsTool::new(vec![])
            .with_local_workers(["local-coder", "local-reviewer"])
            .with_transport(Arc::new(Silent));
        let output = list(&tool).await;
        assert_eq!(output.total, 2);
        assert_eq!(find(&output, "local-coder").origin, Origin::LocalSpec);
        assert_eq!(find(&output, "local-reviewer").reach, Reach::Startable);
    }

    /// A name the broker also serves is owned by the local spec (PL-D4);
    /// the configured remote is listed as shadowed by it, as `invoke_agent`
    /// dispatches (AGE-1131).
    #[tokio::test]
    async fn a_local_spec_owns_a_name_a_configured_remote_shares() {
        let directory = directory(vec![entry("voucher-agent", "", AgentOrigin::Local)]);
        let tool = ListAgentsTool::new(vec![make_agent(
            "voucher-agent",
            "https://hive.dev/a2a/voucher",
            true,
        )])
        .with_local_workers(["voucher-agent"])
        .with_transport(directory);

        let output = list(&tool).await;
        assert_eq!(output.total, 2, "{:?}", output.agents);
        assert_eq!(output.agents[0].origin, Origin::LocalSpec);
        assert_eq!(output.agents[0].shadowed_by, None);
        assert_eq!(output.agents[1].origin, Origin::Remote);
        assert_eq!(output.agents[1].runs_on, AgentOrigin::RemoteConfigured);
        assert_eq!(output.agents[1].shadowed_by, Some(Origin::LocalSpec));
    }

    /// A node cannot be called by name, so a configured remote that shares
    /// its name owns it: that is what `invoke_agent` reaches (AGE-1131).
    #[tokio::test]
    async fn a_configured_remote_owns_a_name_a_node_shares() {
        let directory = directory(vec![entry("voucher-agent-0", "", AgentOrigin::Local)]);
        let tool = ListAgentsTool::new(vec![make_agent(
            "voucher-agent-0",
            "https://hive.dev/a2a/voucher",
            true,
        )])
        .with_transport(directory);

        let output = list(&tool).await;
        assert_eq!(output.agents[0].origin, Origin::Remote);
        assert_eq!(output.agents[0].shadowed_by, None);
        assert_eq!(output.agents[1].origin, Origin::Node);
        assert_eq!(output.agents[1].shadowed_by, Some(Origin::Remote));
    }

    /// A broker that knows its root's spec: `None` is a bare root.
    struct RootedBroker(Option<AgentSpec>);

    #[async_trait::async_trait]
    impl LazyBroker for RootedBroker {
        fn bound_sockets(&self) -> Vec<PathBuf> {
            Vec::new()
        }

        fn root_spec(&self) -> Option<AgentSpec> {
            self.0.clone()
        }
    }

    /// A bare `chatty` root runs as a spec with `delegates_to = ["*"]`, but
    /// its broker checks it as a bare root (AGE-745), which may call an
    /// unexposed spec: `list_agents` offers it too. A root started as a
    /// named spec is checked as that spec by both, and does not see it.
    #[tokio::test]
    async fn a_bare_root_lists_an_unexposed_spec_a_named_caller_does_not() {
        let workspace = workspace_with(&[(
            "hidden-reviewer",
            "[agent]\nname = \"hidden-reviewer\"\n\n[swarm]\nexposed = false\n",
        )]);
        let mut chatty = AgentSpec::named("chatty");
        chatty.swarm.delegates_to = vec!["*".to_string()];
        let mut lead = AgentSpec::named("lead");
        lead.swarm.delegates_to = vec!["*".to_string()];
        let tool = |root: Option<AgentSpec>| {
            let broker: Arc<dyn LazyBroker> = Arc::new(RootedBroker(root));
            ListAgentsTool::new(vec![])
                .with_local_workers(["hidden-reviewer"])
                .with_spec_dirs(Some(workspace.path().to_path_buf()), None)
                .with_caller(caller_policy(Some(&broker), Some(chatty.clone())))
        };

        assert_eq!(names(&list(&tool(None)).await), ["hidden-reviewer"]);
        assert!(names(&list(&tool(Some(lead.clone()))).await).is_empty());
        // A worker (no broker of its own) is checked as its own spec.
        assert_eq!(caller_policy(None, Some(lead.clone())), Some(lead));
    }

    /// A broker that does not answer is not an error: the tool answers with
    /// what settings know.
    #[tokio::test]
    async fn a_broker_that_does_not_answer_leaves_the_settings_listing_alone() {
        let tool = ListAgentsTool::new(vec![make_agent("remote", "https://example.com/a2a", true)])
            .with_transport(Arc::new(Silent));

        let output = list(&tool).await;
        assert_eq!(output.total, 1);
        assert_eq!(find(&output, "remote").origin, Origin::Remote);
    }

    #[test]
    fn a_node_runs_as_its_name_less_the_broker_s_suffix() {
        assert_eq!(node_spec("local-reviewer-0"), Some("local-reviewer"));
        assert_eq!(node_spec("a-b-12"), Some("a-b"));
        assert_eq!(node_spec("local-reviewer"), None);
        assert_eq!(node_spec("-3"), None);
        assert_eq!(node_spec("x-"), None);
    }

    /// A broker whose directory is `agents`, as the gateway's aggregated
    /// card lists them.
    fn directory(agents: Vec<AgentEntry>) -> Arc<dyn Transport> {
        Arc::new(BrokerDirectory(agents))
    }

    struct BrokerDirectory(Vec<AgentEntry>);

    #[async_trait::async_trait]
    impl Transport for BrokerDirectory {
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
