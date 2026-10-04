//! The roster: one read model of every agent a caller can reach (PL-S1
//! RO-1, vault `fabric-roster.md` §3.1).
//!
//! Five origins feed it, one [`RosterSource`] each:
//!
//! | Origin | Source | Reach |
//! | -- | -- | -- |
//! | [`Origin::Handle`] | [`HandleSource`] over a [`HandleIndex`] | `Live` or `Idle` |
//! | [`Origin::Node`] | [`DirectorySource`]: `Directory::in_scope` | `Live`, `Idle`, `Unreachable` |
//! | [`Origin::LocalSpec`] | [`LocalSpecSource`]: the agent-spec loader | `Startable` |
//! | [`Origin::Hosted`] | [`HostedSource`]: nothing until PL-S6 | — |
//! | [`Origin::Remote`] | [`RemoteSource`]: Settings → A2A Agents | `Live`, or `Unreachable` when disabled |
//!
//! A name two origins share goes to the higher one, `Handle > Node >
//! LocalSpec > Hosted > Remote` (PL-D4's answer, 2026-09-27). The loser is
//! kept, with [`RosterEntry::shadowed_by`] naming the origin that owns the
//! name: shadowed, never hidden.
//!
//! Live cards come from the broker's directory through [`LiveCards`], cached
//! for [`LiveCards::TTL`]. One failed fetch turns every node that needed a
//! card [`Reach::Unreachable`] until the next fetch.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chatty_fabric::wire::AgentEntry;
use chatty_fabric::{
    AgentOrigin, CallEvent, CallRequest, CallResult, ConversationScope, Directory, Node,
    NodeState, Transport,
};
use serde::Serialize;

use crate::agent_spec::{AgentSpec, Grant, load_agent_spec_from};
use crate::services::lazy_broker::LazyBroker;
use crate::settings::models::a2a_store::A2aAgentConfig;

/// Where a roster entry comes from. Declared in precedence order, lowest
/// first, so `Ord` is the precedence a name collision is settled by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// A remote A2A agent configured in settings.
    Remote,
    /// An agent on the user's hosted account (PL-S6).
    Hosted,
    /// An agent spec this machine can start.
    LocalSpec,
    /// A running node in this conversation's swarm.
    Node,
    /// A resumable handle in this conversation's swarm.
    Handle,
}

impl Origin {
    pub fn as_str(self) -> &'static str {
        match self {
            Origin::Remote => "remote",
            Origin::Hosted => "hosted",
            Origin::LocalSpec => "local_spec",
            Origin::Node => "node",
            Origin::Handle => "handle",
        }
    }
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Whether a call to an entry can be served, and how.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "reach")]
pub enum Reach {
    /// Something is serving it now.
    Live,
    /// Nothing runs it yet; a call starts it.
    Startable,
    /// Alive, with no run.
    Idle,
    /// A call would fail, and `reason` says why.
    Unreachable { reason: String },
}

/// One plugin of a local agent's spec, as its card shows it (PL-U4): what
/// the agent granted it. A capability it was not granted is refused when
/// the plugin calls it.
#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
pub struct PluginGrants {
    pub module: String,
    /// Granted capabilities by WIT name (`file:<root>` for a file root),
    /// always with `logging`.
    pub grants: Vec<String>,
}

impl PluginGrants {
    pub fn of(spec: &AgentSpec) -> Vec<Self> {
        spec.plugins
            .iter()
            .map(|plugin| {
                let grants = std::iter::once(Grant::Logging)
                    .chain(plugin.grants.iter().cloned())
                    .collect::<BTreeSet<_>>()
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

/// What the roster says about an agent: the summary `list_agents` shows.
/// RO-2 replaces it with the A2A feature card.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RosterCard {
    pub description: String,
    /// Whose machine it runs on (ADR-0011 C5).
    pub agent_origin: AgentOrigin,
    /// Present for a configured remote agent.
    pub url: Option<String>,
    /// `false` only for a remote agent the user disabled.
    pub enabled: bool,
    /// Skills it advertises, when it says.
    pub skills: Vec<String>,
    /// `true` if an API key is configured for it (the value is never kept).
    pub has_api_key: bool,
    /// For a local agent with a spec: its plugins and what each may reach.
    pub plugins: Vec<PluginGrants>,
}

impl RosterCard {
    /// A card that says nothing but `description`, for an agent on this
    /// machine.
    fn local(description: impl Into<String>) -> Self {
        Self {
            description: description.into(),
            agent_origin: AgentOrigin::Local,
            url: None,
            enabled: true,
            skills: Vec::new(),
            has_api_key: false,
            plugins: Vec::new(),
        }
    }

    /// The card the broker's directory serves for an agent.
    fn from_entry(entry: &AgentEntry) -> Self {
        Self {
            description: entry.description.clone(),
            agent_origin: entry.origin,
            url: None,
            enabled: true,
            skills: entry.skills.iter().map(|skill| skill.name.clone()).collect(),
            has_api_key: false,
            plugins: Vec::new(),
        }
    }
}

/// What a resumable handle is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HandleState {
    /// Its worker is serving a run.
    Live,
    /// Saved, with no worker; a call restores it.
    Parked,
}

/// A handle's row (RO-4 shows it in `list_agents`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HandleInfo {
    /// The node that owns it: only it and its ancestors may task it.
    pub owner: String,
    pub state: HandleState,
    pub pending_messages: u32,
    pub behind_by: Option<u32>,
    pub idle_for_s: u64,
}

/// One agent a caller can reach.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RosterEntry {
    pub name: String,
    pub origin: Origin,
    pub reach: Reach,
    pub card: RosterCard,
    /// `Some` for [`Origin::Handle`].
    pub handle: Option<HandleInfo>,
    /// The origin that owns this name, when it is a higher one than this
    /// entry's.
    pub shadowed_by: Option<Origin>,
}

impl RosterEntry {
    fn new(name: impl Into<String>, origin: Origin, reach: Reach, card: RosterCard) -> Self {
        Self {
            name: name.into(),
            origin,
            reach,
            card,
            handle: None,
            shadowed_by: None,
        }
    }
}

/// Who is asking. RO-3 filters by the caller's spec; RO-1 scopes by its
/// conversation.
#[derive(Debug, Clone, Copy)]
pub struct CallerView<'a> {
    pub scope: &'a ConversationScope,
}

/// One origin's entries.
#[async_trait]
pub trait RosterSource: Send + Sync {
    async fn entries(&self, scope: &ConversationScope) -> Vec<RosterEntry>;
}

/// Every source's entries as one list.
pub struct Roster {
    sources: Vec<Arc<dyn RosterSource>>,
}

impl Roster {
    pub fn new(sources: Vec<Arc<dyn RosterSource>>) -> Self {
        Self { sources }
    }

    /// Every entry `caller` can see, in name order; on a shared name the
    /// owner first, then the shadowed ones from the highest origin down.
    pub async fn for_caller(&self, caller: &CallerView<'_>) -> Vec<RosterEntry> {
        let mut by_name: BTreeMap<String, Vec<RosterEntry>> = BTreeMap::new();
        for source in &self.sources {
            for entry in source.entries(caller.scope).await {
                by_name.entry(entry.name.clone()).or_default().push(entry);
            }
        }
        by_name
            .into_values()
            .flat_map(|mut entries| {
                // Stable: of two entries of one origin, the first source's
                // owns the name.
                entries.sort_by(|a, b| b.origin.cmp(&a.origin));
                let owner = entries[0].origin;
                for loser in entries.iter_mut().skip(1) {
                    loser.shadowed_by = Some(owner);
                }
                entries
            })
            .collect()
    }
}

// ── Live cards ──────────────────────────────────────────────────────────────

/// What time it is, so the card cache can be tested without sleeping.
pub trait Clock: Send + Sync {
    fn now(&self) -> Instant;
}

/// The real clock.
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// Why the broker's directory could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchError {
    /// The broker is configured but did not start (AGE-746).
    BrokerDidNotStart(String),
    /// The broker did not answer.
    NoAnswer(String),
}

impl fmt::Display for FetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FetchError::BrokerDidNotStart(error) => write!(f, "the broker did not start: {error}"),
            FetchError::NoAnswer(error) => write!(f, "the broker did not answer: {error}"),
        }
    }
}

/// One read of every card the broker serves.
#[async_trait]
pub trait CardFetch: Send + Sync {
    async fn fetch(&self) -> Result<Vec<AgentEntry>, FetchError>;
}

/// The broker's directory over the fabric: a worker's connection, or the
/// in-process root's direct handle.
pub struct TransportCards(pub Arc<dyn Transport>);

#[async_trait]
impl CardFetch for TransportCards {
    async fn fetch(&self) -> Result<Vec<AgentEntry>, FetchError> {
        directory_over(self.0.as_ref()).await
    }
}

/// A broker started on first use (BI-2): the fetch starts it.
pub struct LazyBrokerCards(pub Arc<dyn LazyBroker>);

#[async_trait]
impl CardFetch for LazyBrokerCards {
    async fn fetch(&self) -> Result<Vec<AgentEntry>, FetchError> {
        match self.0.transport().await {
            Ok(Some(transport)) => directory_over(transport.as_ref()).await,
            Ok(None) => Ok(Vec::new()),
            Err(error) => {
                tracing::warn!(%error, "Failed to start the broker for the roster");
                Err(FetchError::BrokerDidNotStart(error.to_string()))
            }
        }
    }
}

async fn directory_over(transport: &dyn Transport) -> Result<Vec<AgentEntry>, FetchError> {
    use futures::StreamExt;

    let mut stream = transport
        .call(CallRequest::ListAgents)
        .await
        .map_err(|error| FetchError::NoAnswer(error.to_string()))?;
    while let Some(event) = stream.next().await {
        match event {
            Ok(CallEvent::Result(CallResult::Agents(agents))) => return Ok(agents),
            Ok(CallEvent::Result(other)) => {
                return Err(FetchError::NoAnswer(format!(
                    "not a directory: {other:?}"
                )));
            }
            Ok(_) => {}
            Err(error) => return Err(FetchError::NoAnswer(error.to_string())),
        }
    }
    Err(FetchError::NoAnswer("the call ended without a result".into()))
}

type CachedCards = Result<Arc<Vec<AgentEntry>>, FetchError>;

/// The broker's cards, fetched at most once per [`ttl`](Self::TTL). A
/// failed fetch is cached too: a dead broker is not asked again until the
/// TTL runs out.
pub struct LiveCards {
    fetch: Arc<dyn CardFetch>,
    clock: Arc<dyn Clock>,
    ttl: Duration,
    cache: tokio::sync::Mutex<Option<(Instant, CachedCards)>>,
}

impl LiveCards {
    pub const TTL: Duration = Duration::from_secs(30);

    pub fn new(fetch: Arc<dyn CardFetch>) -> Self {
        Self::with_clock(fetch, Arc::new(SystemClock), Self::TTL)
    }

    pub fn with_clock(fetch: Arc<dyn CardFetch>, clock: Arc<dyn Clock>, ttl: Duration) -> Self {
        Self {
            fetch,
            clock,
            ttl,
            cache: tokio::sync::Mutex::new(None),
        }
    }

    /// Every card the broker serves, from the cache while it is fresh.
    pub async fn get(&self) -> CachedCards {
        // Held across the fetch, so callers that arrive together share one.
        let mut cache = self.cache.lock().await;
        let now = self.clock.now();
        if let Some((at, cards)) = cache.as_ref()
            && now.saturating_duration_since(*at) < self.ttl
        {
            return cards.clone();
        }
        let cards = self.fetch.fetch().await.map(Arc::new);
        *cache = Some((now, cards.clone()));
        cards
    }

    async fn card(&self, name: &str) -> Result<Option<AgentEntry>, FetchError> {
        Ok(self
            .get()
            .await?
            .iter()
            .find(|entry| entry.name == name)
            .cloned())
    }
}

// ── Sources ─────────────────────────────────────────────────────────────────

/// The stand-in description of a local agent whose spec says none.
const LOCAL_STAND_IN: &str =
    "A chatty agent in its own process and its own workspace. Delegate a self-contained task to it.";

/// Agent specs this machine can start (PL-U1's loader). Listed whether or
/// not a broker runs: a startable spec needs no process.
pub struct LocalSpecSource {
    specs: Vec<AgentSpec>,
    cards: Option<Arc<LiveCards>>,
}

impl LocalSpecSource {
    /// `specs`, with the broker's card for each when it serves one: it says
    /// what the spec resolved to (its model, its tools; ADR-0011 C10).
    pub fn new(specs: Vec<AgentSpec>, cards: Option<Arc<LiveCards>>) -> Self {
        Self { specs, cards }
    }

    /// The specs named `names`, looked up under `workspace` and `data_dir`
    /// (then the presets). A name with no spec file — the default
    /// `local-agent` — is a bare spec.
    pub fn load(
        names: &[String],
        workspace: Option<&Path>,
        data_dir: Option<&Path>,
        cards: Option<Arc<LiveCards>>,
    ) -> Self {
        let specs = names
            .iter()
            .map(|name| match load_agent_spec_from(name, workspace, data_dir) {
                Ok(loaded) => loaded.spec,
                Err(_) => AgentSpec::named(name),
            })
            .collect();
        Self::new(specs, cards)
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.specs.iter().map(|spec| spec.agent.name.as_str())
    }
}

#[async_trait]
impl RosterSource for LocalSpecSource {
    async fn entries(&self, _scope: &ConversationScope) -> Vec<RosterEntry> {
        let mut entries = Vec::with_capacity(self.specs.len());
        for spec in &self.specs {
            let name = &spec.agent.name;
            let live = match &self.cards {
                Some(cards) => cards.card(name).await.ok().flatten(),
                None => None,
            };
            let mut card = match live {
                Some(entry) => RosterCard::from_entry(&entry),
                None => RosterCard::local(
                    spec.agent
                        .description
                        .clone()
                        .unwrap_or_else(|| LOCAL_STAND_IN.to_string()),
                ),
            };
            card.plugins = PluginGrants::of(spec);
            entries.push(RosterEntry::new(
                name.clone(),
                Origin::LocalSpec,
                Reach::Startable,
                card,
            ));
        }
        entries
    }
}

/// The nodes a broker admitted, read by conversation.
pub trait NodeTable: Send + Sync {
    fn in_scope(&self, scope: &ConversationScope) -> Vec<Node>;
}

impl NodeTable for Mutex<Directory> {
    fn in_scope(&self, scope: &ConversationScope) -> Vec<Node> {
        self.lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .in_scope(scope)
            .cloned()
            .collect()
    }
}

/// Running nodes in the caller's swarm.
///
/// With a [`NodeTable`], the nodes are `Directory::in_scope(caller.scope)`
/// and each one's card comes from [`LiveCards`]: a node whose card is not
/// served, or whose broker did not answer, is `Unreachable`. Without one
/// — a host that reaches its broker only over the fabric — the nodes are
/// the broker's own answer, which it already scoped, less the specs it
/// serves (those are [`LocalSpecSource`]'s).
pub struct DirectorySource {
    table: Option<Arc<dyn NodeTable>>,
    cards: Arc<LiveCards>,
    specs: BTreeSet<String>,
}

impl DirectorySource {
    pub fn new(cards: Arc<LiveCards>, specs: impl IntoIterator<Item = String>) -> Self {
        Self {
            table: None,
            cards,
            specs: specs.into_iter().collect(),
        }
    }

    pub fn with_table(mut self, table: Arc<dyn NodeTable>) -> Self {
        self.table = Some(table);
        self
    }

    async fn from_table(&self, table: &dyn NodeTable, scope: &ConversationScope) -> Vec<RosterEntry> {
        let fetched = self.cards.get().await;
        table
            .in_scope(scope)
            .into_iter()
            .filter(|node| node.state() != NodeState::Ended)
            .map(|node| {
                let name = node.name().as_str();
                let card = match &fetched {
                    Ok(cards) => cards.iter().find(|entry| entry.name == name),
                    Err(_) => None,
                };
                let reach = match (&fetched, card, node.state()) {
                    (Err(error), _, _) => Reach::Unreachable {
                        reason: error.to_string(),
                    },
                    (Ok(_), None, NodeState::Starting) => Reach::Live,
                    (Ok(_), None, _) => Reach::Unreachable {
                        reason: format!("the broker serves no card for '{name}'"),
                    },
                    (Ok(_), Some(_), NodeState::Idle) => Reach::Idle,
                    (Ok(_), Some(_), _) => Reach::Live,
                };
                let card = card
                    .map(RosterCard::from_entry)
                    .unwrap_or_else(|| RosterCard::local(format!("A '{}' node", node.spec())));
                RosterEntry::new(name, Origin::Node, reach, card)
            })
            .collect()
    }
}

#[async_trait]
impl RosterSource for DirectorySource {
    async fn entries(&self, scope: &ConversationScope) -> Vec<RosterEntry> {
        if let Some(table) = &self.table {
            return self.from_table(table.as_ref(), scope).await;
        }
        let Ok(cards) = self.cards.get().await else {
            return Vec::new();
        };
        cards
            .iter()
            // An entry with no name cannot be addressed.
            .filter(|entry| !entry.name.is_empty() && !self.specs.contains(&entry.name))
            .map(|entry| {
                RosterEntry::new(
                    entry.name.clone(),
                    Origin::Node,
                    Reach::Live,
                    RosterCard::from_entry(entry),
                )
            })
            .collect()
    }
}

/// The resumable handles a conversation's swarm holds (resumable
/// conversations, RC-3). Nothing implements it outside tests until then.
pub trait HandleIndex: Send + Sync {
    fn handles(&self, scope: &ConversationScope) -> Vec<(String, HandleInfo)>;
}

/// Resumable handles in the caller's swarm.
pub struct HandleSource {
    index: Arc<dyn HandleIndex>,
}

impl HandleSource {
    pub fn new(index: Arc<dyn HandleIndex>) -> Self {
        Self { index }
    }
}

#[async_trait]
impl RosterSource for HandleSource {
    async fn entries(&self, scope: &ConversationScope) -> Vec<RosterEntry> {
        self.index
            .handles(scope)
            .into_iter()
            .map(|(name, info)| {
                let reach = match info.state {
                    HandleState::Live => Reach::Live,
                    HandleState::Parked => Reach::Idle,
                };
                let mut entry = RosterEntry::new(
                    name,
                    Origin::Handle,
                    reach,
                    RosterCard::local(format!("A resumable handle owned by {}", info.owner)),
                );
                entry.handle = Some(info);
                entry
            })
            .collect()
    }
}

/// Remote A2A agents from Settings → A2A Agents.
pub struct RemoteSource {
    agents: Vec<A2aAgentConfig>,
}

impl RemoteSource {
    pub fn new(agents: Vec<A2aAgentConfig>) -> Self {
        Self { agents }
    }
}

#[async_trait]
impl RosterSource for RemoteSource {
    async fn entries(&self, _scope: &ConversationScope) -> Vec<RosterEntry> {
        self.agents
            .iter()
            .map(|agent| {
                let reach = if agent.enabled {
                    Reach::Live
                } else {
                    Reach::Unreachable {
                        reason: "disabled in Settings".to_string(),
                    }
                };
                RosterEntry::new(
                    agent.name.clone(),
                    Origin::Remote,
                    reach,
                    RosterCard {
                        description: format!("Remote A2A agent at {}", agent.url),
                        agent_origin: AgentOrigin::RemoteConfigured,
                        url: Some(agent.url.clone()),
                        enabled: agent.enabled,
                        skills: agent.skills.clone(),
                        has_api_key: agent.has_api_key(),
                        plugins: Vec::new(),
                    },
                )
            })
            .collect()
    }
}

/// Agents on the user's hosted account: none until PL-S6.
pub struct HostedSource;

#[async_trait]
impl RosterSource for HostedSource {
    async fn entries(&self, _scope: &ConversationScope) -> Vec<RosterEntry> {
        Vec::new()
    }
}

#[cfg(test)]
#[path = "roster_tests.rs"]
mod tests;
