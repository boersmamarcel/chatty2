//! One turn's swarm as a tree of agents (TB-2, AGE-664).
//!
//! The root session hears its own turn as [`SessionEvent`]s and every run
//! under its delegations as the broker's tagged batches
//! ([`SessionEvent::SwarmEvent`], TB-1): `(root_task_id, node, chain)` from
//! the broker's task table, never from the worker. [`SwarmTrace`] folds both
//! into a [`Tree`] of [`AgentNode`]s rooted at the human turn — who ran,
//! under whom, on which model, which tools each called and what each spent —
//! and the broker's edge log ([`EdgeRow`]) settles what the events cannot:
//! the node the root's callee ran as, which of two same-spec siblings a run
//! hangs under, and the calls that were refused before anything ran.
//!
//! The root's own callee is not forwarded (TB-1): it reports through its
//! delegation ([`SessionEvent::Delegation`]). Its node comes from `Started`,
//! its usage from `Finished`, and its tool calls from the step lines, which
//! carry its descendants' steps as well; the lines its descendants' batches
//! account for are theirs (`settle_steps`). Delegations are told apart by
//! order alone, so progress goes to the latest one not finished.
//!
//! # Live
//!
//! A frontend builds the tree while the turn streams: [`SwarmTrace::new`],
//! then [`apply`](SwarmTrace::apply) for every event the session emits, in
//! order. Each call leaves a consistent tree; [`revision`](SwarmTrace::revision)
//! moves whenever it changed, so a view redraws only then. After the turn,
//! [`apply_edge`](SwarmTrace::apply_edge) folds in the edge log's rows;
//! [`SwarmTrace::from_edges`] is the same for a finished turn in one call.
//!
//! # Spend
//!
//! A run reports its usage once, on its terminal status, and a sub-leader
//! has already folded its workers' usage into it (AGE-415): what a node
//! reports is its whole subtree's. A node's own spend is what it reported
//! less what its children reported, per model, so a sub-leader is never
//! billed its workers' tokens twice. The root's own spend is its turn's
//! usage and its plugins' lines. The tree's [`total`](SwarmTrace::total) is
//! every node's own spend summed; it equals [`billed`](SwarmTrace::billed),
//! the root's own lines plus the lines its delegations reported, which is
//! what the conversation records. Lines carry facts — model, time — and are
//! priced on read with [`price`](crate::models::token_usage::price)
//! (AGE-682).
//!
//! This lives in chatty-core, not `crates/chatty-trace`, whose trajectory
//! types are human-reserved (`RESERVED.md`, AGE-5).

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chatty_fabric::{EdgeKind, EdgeRow, ROOT_NAME, SwarmEvent, SwarmItem};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::models::token_usage::{ModelRef, TokenUsage};
use crate::services::a2a_client::{USAGE_METADATA_KEY, usage_from_status_metadata};
use crate::session::SessionEvent;
use crate::tools::invoke_agent_tool::InvokeAgentProgress;
use crate::tools::plugin_tool::PLUGIN_TOOL_SEPARATOR;

/// A node's place in a [`Tree`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(usize);

#[derive(Debug, Clone)]
struct Slot<T> {
    value: T,
    parent: Option<NodeId>,
    children: Vec<NodeId>,
}

/// A rooted tree whose children keep the order they were added in.
///
/// Two trees are equal when their shapes and values are, whatever order the
/// nodes were stored in.
#[derive(Debug, Clone)]
pub struct Tree<T> {
    slots: Vec<Slot<T>>,
}

impl<T> Tree<T> {
    pub fn new(root: T) -> Self {
        Self {
            slots: vec![Slot {
                value: root,
                parent: None,
                children: Vec::new(),
            }],
        }
    }

    pub fn root(&self) -> NodeId {
        NodeId(0)
    }

    pub fn get(&self, id: NodeId) -> &T {
        &self.slots[id.0].value
    }

    pub(crate) fn get_mut(&mut self, id: NodeId) -> &mut T {
        &mut self.slots[id.0].value
    }

    pub fn parent(&self, id: NodeId) -> Option<NodeId> {
        self.slots[id.0].parent
    }

    pub fn children(&self, id: NodeId) -> &[NodeId] {
        &self.slots[id.0].children
    }

    /// The number of nodes, the root included; never zero.
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    /// Always `false`: a tree has its root.
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// How many edges `id` is below the root.
    pub fn depth(&self, id: NodeId) -> usize {
        std::iter::successors(self.parent(id), |p| self.parent(*p)).count()
    }

    /// Add `value` as `parent`'s last child.
    pub fn push(&mut self, parent: NodeId, value: T) -> NodeId {
        let id = NodeId(self.slots.len());
        self.slots.push(Slot {
            value,
            parent: Some(parent),
            children: Vec::new(),
        });
        self.slots[parent.0].children.push(id);
        id
    }

    /// Move `id`, with its subtree, to be `parent`'s last child.
    fn move_under(&mut self, id: NodeId, parent: NodeId) {
        if let Some(old) = self.slots[id.0].parent {
            self.slots[old.0].children.retain(|c| *c != id);
        }
        self.slots[id.0].parent = Some(parent);
        self.slots[parent.0].children.push(id);
    }

    /// Every node, parents before children, siblings in order.
    pub fn preorder(&self) -> Vec<NodeId> {
        let mut order = Vec::with_capacity(self.slots.len());
        let mut stack = vec![self.root()];
        while let Some(id) = stack.pop() {
            order.push(id);
            stack.extend(self.children(id).iter().rev());
        }
        order
    }

    fn same_subtree(&self, id: NodeId, other: &Self, other_id: NodeId) -> bool
    where
        T: PartialEq,
    {
        let (mine, theirs) = (self.children(id), other.children(other_id));
        self.get(id) == other.get(other_id)
            && mine.len() == theirs.len()
            && mine
                .iter()
                .zip(theirs)
                .all(|(a, b)| self.same_subtree(*a, other, *b))
    }
}

impl<T: PartialEq> PartialEq for Tree<T> {
    fn eq(&self, other: &Self) -> bool {
        self.same_subtree(self.root(), other, other.root())
    }
}

/// One agent in the swarm: the root's turn, or a run under it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentNode {
    /// The name the broker admitted the run's connection under
    /// (`<spec>-<n>`); [`ROOT_NAME`] for the root. A refused call, which
    /// never ran, is named by the spec it asked for.
    pub name: String,
    /// The spec it ran as.
    pub spec: String,
    /// The root call it descends from; `None` for the root, which may make
    /// several.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_task_id: Option<String>,
    /// The model it spent most on, as its usage lines name it; `None`
    /// until it reports usage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelRef>,
    pub turns: u32,
    /// How long its answer is. The text stays with its caller; only its
    /// length reaches the root (TB-1).
    pub text_bytes: u64,
    pub tool_calls: Vec<ToolCall>,
    /// Its own spend, one line per model (and plugin): what it reported
    /// less what its children did. See the module docs.
    pub usage: Vec<UsageLine>,
    pub status: NodeStatus,
}

impl AgentNode {
    fn new(name: &str, spec: &str, root_task_id: Option<&str>) -> Self {
        Self {
            name: name.to_string(),
            spec: spec.to_string(),
            root_task_id: root_task_id.map(str::to_string),
            model: None,
            turns: 0,
            text_bytes: 0,
            tool_calls: Vec::new(),
            usage: Vec::new(),
            status: NodeStatus::Running,
        }
    }

    /// Its own spend as usage lines, for
    /// [`price`](crate::models::token_usage::price).
    pub fn token_usage(&self) -> Vec<TokenUsage> {
        self.usage.iter().map(UsageLine::to_token_usage).collect()
    }

    fn tool_call_mut(&mut self, id: &str) -> Option<&mut ToolCall> {
        self.tool_calls.iter_mut().rev().find(|call| call.id == id)
    }

    fn start_tool(&mut self, id: &str, name: &str) {
        self.tool_calls.push(ToolCall {
            id: id.to_string(),
            name: name.to_string(),
            plugin: plugin_of(name),
            arguments: None,
            outcome: ToolOutcome::Running,
        });
    }

    fn end_tool(&mut self, id: &str, outcome: ToolOutcome) {
        if let Some(call) = self.tool_call_mut(id) {
            call.outcome = outcome;
        }
    }
}

/// One tool call an agent made.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// The WASM plugin the tool belongs to, for a plugin's tool
    /// (`<plugin>__<tool>`); `None` for any other.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin: Option<String>,
    /// Its arguments, when they reached the root: the root's own calls'
    /// do, a nested run's never cross a hop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<Value>,
    pub outcome: ToolOutcome,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ToolOutcome {
    Running,
    Done { result: String },
    Failed { error: String },
}

/// Where a node's run is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum NodeStatus {
    Running,
    Completed,
    Failed,
    Canceled,
    /// The call never ran: the broker refused it (cycle, depth, spec), or
    /// the agent rejected the task.
    Refused {
        reason: String,
    },
}

impl NodeStatus {
    /// The status for an A2A task's final state.
    fn ended(state: &str) -> Self {
        match state {
            "completed" => Self::Completed,
            "canceled" => Self::Canceled,
            "rejected" => Self::Refused {
                reason: state.to_string(),
            },
            _ => Self::Failed,
        }
    }

    pub fn is_running(&self) -> bool {
        matches!(self, Self::Running)
    }
}

/// Tokens spent on one model (by one plugin, if any): a fact, never a
/// price.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct UsageLine {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin: Option<String>,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_read_tokens: u32,
    pub cache_write_tokens: u32,
    /// When its last request finished, in Unix milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at_ms: Option<u64>,
}

impl UsageLine {
    /// All four token counts.
    pub fn tokens(&self) -> u64 {
        [
            self.input_tokens,
            self.output_tokens,
            self.cache_read_tokens,
            self.cache_write_tokens,
        ]
        .iter()
        .map(|n| u64::from(*n))
        .sum()
    }

    pub fn to_token_usage(&self) -> TokenUsage {
        TokenUsage {
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            cache_read_tokens: self.cache_read_tokens,
            cache_write_tokens: self.cache_write_tokens,
            model: self.model.clone(),
            plugin: self.plugin.clone(),
            at: self.at_ms.map(|ms| UNIX_EPOCH + Duration::from_millis(ms)),
            ..TokenUsage::default()
        }
    }

    /// `usage` as lines: one per request when it has per-request records,
    /// each at its own model, else one.
    pub fn from_token_usage(usage: &TokenUsage) -> Vec<UsageLine> {
        let ms = |at: Option<SystemTime>| {
            at.and_then(|at| at.duration_since(UNIX_EPOCH).ok())
                .map(|since| since.as_millis() as u64)
        };
        if usage.calls.is_empty() {
            return vec![UsageLine {
                model: usage.model.clone(),
                plugin: usage.plugin.clone(),
                input_tokens: usage.input_tokens,
                output_tokens: usage.output_tokens,
                cache_read_tokens: usage.cache_read_tokens,
                cache_write_tokens: usage.cache_write_tokens,
                at_ms: ms(usage.at),
            }];
        }
        usage
            .calls
            .iter()
            .map(|call| UsageLine {
                model: call.model.clone().or_else(|| usage.model.clone()),
                plugin: usage.plugin.clone(),
                input_tokens: call.input_tokens,
                output_tokens: call.output_tokens,
                cache_read_tokens: call.cache_read_tokens,
                cache_write_tokens: call.cache_write_tokens,
                at_ms: ms(call.at.or(usage.at)),
            })
            .collect()
    }

    fn same_bucket(&self, other: &UsageLine) -> bool {
        self.model == other.model && self.plugin == other.plugin
    }
}

/// `lines` folded to one line per model and plugin, in first-seen order.
pub fn merge_lines<'a>(lines: impl IntoIterator<Item = &'a UsageLine>) -> Vec<UsageLine> {
    let mut merged: Vec<UsageLine> = Vec::new();
    for line in lines {
        match merged.iter_mut().find(|m| m.same_bucket(line)) {
            Some(m) => {
                m.input_tokens = m.input_tokens.saturating_add(line.input_tokens);
                m.output_tokens = m.output_tokens.saturating_add(line.output_tokens);
                m.cache_read_tokens = m.cache_read_tokens.saturating_add(line.cache_read_tokens);
                m.cache_write_tokens = m.cache_write_tokens.saturating_add(line.cache_write_tokens);
                m.at_ms = m.at_ms.max(line.at_ms);
            }
            None => merged.push(line.clone()),
        }
    }
    merged
}

/// `whole` less `part`, per model and plugin; a line left with nothing
/// is dropped.
fn subtract(whole: &[UsageLine], part: &[UsageLine]) -> Vec<UsageLine> {
    let part = merge_lines(part);
    merge_lines(whole)
        .into_iter()
        .map(|mut line| {
            if let Some(p) = part.iter().find(|p| p.same_bucket(&line)) {
                line.input_tokens = line.input_tokens.saturating_sub(p.input_tokens);
                line.output_tokens = line.output_tokens.saturating_sub(p.output_tokens);
                line.cache_read_tokens = line.cache_read_tokens.saturating_sub(p.cache_read_tokens);
                line.cache_write_tokens =
                    line.cache_write_tokens.saturating_sub(p.cache_write_tokens);
            }
            line
        })
        .filter(|line| line.tokens() > 0)
        .collect()
}

/// The model `lines` spent most tokens on.
fn main_model(lines: &[UsageLine]) -> Option<ModelRef> {
    merge_lines(lines.iter().filter(|l| l.plugin.is_none()))
        .into_iter()
        .filter(|l| l.model.is_some())
        .max_by_key(UsageLine::tokens)
        .and_then(|l| l.model)
}

/// The plugin an advertised tool name belongs to (`<plugin>__<tool>`).
pub fn plugin_of(name: &str) -> Option<String> {
    let (plugin, tool) = name.split_once(PLUGIN_TOOL_SEPARATOR)?;
    (!plugin.is_empty() && !tool.is_empty()).then(|| plugin.to_string())
}

/// A tool call's arguments as JSON; text that is not JSON stays text.
fn arguments(raw: &str) -> Option<Value> {
    match serde_json::from_str::<Value>(raw) {
        Ok(Value::Null) => None,
        Ok(value) => Some(value),
        Err(_) => Some(Value::String(raw.to_string())),
    }
}

/// One turn's swarm, built from its events and the broker's edge log.
/// See the module docs.
#[derive(Debug, Clone)]
pub struct SwarmTrace {
    tree: Tree<AgentNode>,
    /// Each run's chain, spec names root first, as the broker stamped it.
    chains: BTreeMap<NodeId, Vec<String>>,
    /// What each run reported spending: its whole subtree's.
    reported: BTreeMap<NodeId, Vec<UsageLine>>,
    /// Runs still named by their spec: stand-ins for runs whose children
    /// reported first, until their own first batch names them, and the
    /// root's callees, until the edge log does.
    unnamed: Vec<NodeId>,
    /// Each of the root's callees and the step lines its delegation
    /// reported, its own and its descendants' mixed (see
    /// [`settle_steps`](Self::settle_steps)).
    callees: BTreeMap<NodeId, Vec<String>>,
    /// The root's delegations that have started and not finished, latest
    /// last.
    open: Vec<NodeId>,
    /// The lines the root's delegations reported, as its conversation
    /// records them.
    delegated: Vec<UsageLine>,
    revision: u64,
}

impl Default for SwarmTrace {
    fn default() -> Self {
        Self::new()
    }
}

impl SwarmTrace {
    /// A turn that has not started: the root, and nothing under it.
    pub fn new() -> Self {
        let root = Tree::new(AgentNode::new(ROOT_NAME, ROOT_NAME, None));
        Self {
            chains: BTreeMap::from([(root.root(), vec![ROOT_NAME.to_string()])]),
            tree: root,
            reported: BTreeMap::new(),
            unnamed: Vec::new(),
            callees: BTreeMap::new(),
            open: Vec::new(),
            delegated: Vec::new(),
            revision: 0,
        }
    }

    /// A finished turn: its root session's `events`, in order, then the
    /// broker's `edges`.
    pub fn from_edges(edges: &[EdgeRow], events: &[SessionEvent]) -> Self {
        let mut trace = Self::new();
        for event in events {
            trace.apply(event);
        }
        for row in edges {
            trace.apply_edge(row);
        }
        trace
    }

    pub fn tree(&self) -> &Tree<AgentNode> {
        &self.tree
    }

    /// Moves every time the tree changes.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// The node named `name`.
    pub fn node_named(&self, name: &str) -> Option<NodeId> {
        self.tree
            .preorder()
            .into_iter()
            .find(|id| self.tree.get(*id).name == name)
    }

    /// The root calls the turn made, in the order their runs first
    /// reported.
    pub fn root_task_ids(&self) -> Vec<&str> {
        let mut ids: Vec<&str> = Vec::new();
        for child in self.tree.children(self.tree.root()) {
            if let Some(id) = self.tree.get(*child).root_task_id.as_deref()
                && !ids.contains(&id)
            {
                ids.push(id);
            }
        }
        ids
    }

    /// Every node's own spend, one line per model and plugin.
    pub fn total(&self) -> Vec<UsageLine> {
        merge_lines(
            self.tree
                .preorder()
                .into_iter()
                .flat_map(|id| self.tree.get(id).usage.iter()),
        )
    }

    /// What the root's conversation is billed for the turn: its own lines
    /// and every line its delegations reported, one per model and plugin.
    pub fn billed(&self) -> Vec<UsageLine> {
        let root = self.tree.get(self.tree.root());
        merge_lines(root.usage.iter().chain(&self.delegated))
    }

    /// Fold in one event of the root's turn.
    pub fn apply(&mut self, event: &SessionEvent) {
        let root_id = self.tree.root();
        let root = self.tree.get_mut(root_id);
        match event {
            SessionEvent::TurnStarted => {
                root.turns += 1;
                root.status = NodeStatus::Running;
            }
            SessionEvent::Text(text) => root.text_bytes += text.len() as u64,
            SessionEvent::ToolCallStarted { id, name } => root.start_tool(id, name),
            SessionEvent::ToolCallInput { id, arguments: raw } => {
                if let Some(call) = root.tool_call_mut(id) {
                    call.arguments = arguments(raw);
                }
            }
            SessionEvent::ToolCallResult { id, result } => root.end_tool(
                id,
                ToolOutcome::Done {
                    result: result.clone(),
                },
            ),
            SessionEvent::ToolCallError { id, error } => root.end_tool(
                id,
                ToolOutcome::Failed {
                    error: error.clone(),
                },
            ),
            SessionEvent::TokenUsage(usage) | SessionEvent::PluginUsage(usage) => {
                let lines = UsageLine::from_token_usage(usage);
                root.usage = merge_lines(root.usage.iter().chain(&lines));
                root.model = main_model(&root.usage);
            }
            SessionEvent::Delegation(progress) => return self.apply_delegation(progress),
            SessionEvent::SwarmEvent(batch) => return self.apply_swarm(batch),
            SessionEvent::Error(_) => root.status = NodeStatus::Failed,
            SessionEvent::Cancelled => root.status = NodeStatus::Canceled,
            SessionEvent::TurnEnded => {
                if root.status.is_running() {
                    root.status = NodeStatus::Completed;
                }
            }
            _ => return,
        }
        self.revision += 1;
    }

    /// Fold in the root's own callee's progress. The broker does not
    /// forward the root's callee (TB-1): it reports through its
    /// delegation, whose steps carry its descendants' steps as well.
    fn apply_delegation(&mut self, progress: &InvokeAgentProgress) {
        match progress {
            InvokeAgentProgress::Started { agent_name, .. } => {
                let id = self.callee(agent_name);
                self.open.push(id);
            }
            InvokeAgentProgress::Text(text) => {
                let Some(id) = self.open.last().copied() else {
                    return;
                };
                self.tree.get_mut(id).text_bytes += text.len() as u64;
            }
            InvokeAgentProgress::Step(step) => {
                let Some(id) = self.open.last().copied() else {
                    return;
                };
                self.callees.entry(id).or_default().push(step.clone());
                self.settle_steps(id);
            }
            InvokeAgentProgress::Finished { success, usage, .. } => {
                let lines: Vec<UsageLine> =
                    usage.iter().flat_map(UsageLine::from_token_usage).collect();
                self.delegated = merge_lines(self.delegated.iter().chain(&lines));
                if let Some(id) = self.open.pop() {
                    self.reported.insert(id, lines);
                    self.settle_usage(id);
                    let node = self.tree.get_mut(id);
                    if node.status.is_running() {
                        node.status = if *success {
                            NodeStatus::Completed
                        } else {
                            NodeStatus::Failed
                        };
                    }
                }
            }
            InvokeAgentProgress::Swarm(batch) => return self.apply_swarm(batch),
        }
        self.revision += 1;
    }

    /// The node for a delegation of the root's to `spec`: a stand-in its
    /// descendants already left, or a new one.
    fn callee(&mut self, spec: &str) -> NodeId {
        let chain = vec![ROOT_NAME.to_string(), spec.to_string()];
        let root = self.tree.root();
        let stand_in = self.tree.children(root).iter().copied().find(|id| {
            self.unnamed.contains(id)
                && !self.callees.contains_key(id)
                && self.chains.get(id) == Some(&chain)
        });
        let id = stand_in.unwrap_or_else(|| {
            let id = self.tree.push(root, AgentNode::new(spec, spec, None));
            self.chains.insert(id, chain);
            self.unnamed.push(id);
            id
        });
        let node = self.tree.get_mut(id);
        // A delegated task is one turn of its worker.
        node.turns = node.turns.max(1);
        self.callees.insert(id, Vec::new());
        id
    }

    /// Fold in one of the broker's batches.
    pub fn apply_swarm(&mut self, batch: &SwarmEvent) {
        let id = self.node_for(batch);
        for item in &batch.inner {
            let node = self.tree.get_mut(id);
            match item {
                SwarmItem::TurnStarted => node.turns += 1,
                SwarmItem::Text { bytes } => node.text_bytes += bytes,
                SwarmItem::ToolCallStarted { id, name } => node.start_tool(id, name),
                SwarmItem::ToolCallResult { id, result } => node.end_tool(
                    id,
                    ToolOutcome::Done {
                        result: result.clone(),
                    },
                ),
                SwarmItem::ToolCallError { id, error } => node.end_tool(
                    id,
                    ToolOutcome::Failed {
                        error: error.clone(),
                    },
                ),
                SwarmItem::Usage { usage } => {
                    let metadata = serde_json::json!({ USAGE_METADATA_KEY: usage });
                    let lines = usage_from_status_metadata(Some(&metadata))
                        .iter()
                        .flat_map(UsageLine::from_token_usage)
                        .collect();
                    self.reported.insert(id, lines);
                    self.settle_usage(id);
                    if let Some(parent) = self.tree.parent(id) {
                        self.settle_usage(parent);
                    }
                }
                SwarmItem::Ended { state } => node.status = NodeStatus::ended(state),
            }
        }
        if let Some(callee) = self.callee_above(id) {
            self.settle_steps(callee);
        }
        self.revision += 1;
    }

    /// Fold in one row of the broker's edge log. A row about a run this
    /// trace does not hold — another turn's — changes nothing.
    pub fn apply_edge(&mut self, row: &EdgeRow) {
        let root = self.tree.root();
        match row.kind {
            EdgeKind::Task if row.from == ROOT_NAME => {
                // The root's callee is named by its spec until now.
                let Some(id) = self.tree.children(root).iter().copied().find(|id| {
                    let node = self.tree.get(*id);
                    node.name == row.to
                        || (self.unnamed.contains(id) && is_node_of(&row.to, &node.spec))
                }) else {
                    return;
                };
                self.name(id, &row.to);
                let node = self.tree.get_mut(id);
                if node.status.is_running() {
                    node.status = NodeStatus::ended(&row.outcome);
                }
            }
            EdgeKind::Task => {
                let Some(to) = self.node_named(&row.to) else {
                    return;
                };
                let from = match self.node_named(&row.from) {
                    Some(from) => from,
                    // The caller is the root's callee, still named by its
                    // spec: its callee's row names it.
                    None => match self.tree.parent(to).filter(|parent| {
                        self.unnamed.contains(parent)
                            && is_node_of(&row.from, &self.tree.get(*parent).spec)
                    }) {
                        Some(parent) => {
                            self.name(parent, &row.from);
                            parent
                        }
                        None => return,
                    },
                };
                let old = self.tree.parent(to);
                if from != to && old != Some(from) && !self.is_below(from, to) {
                    self.tree.move_under(to, from);
                    for id in old.into_iter().chain([from]) {
                        self.settle_usage(id);
                    }
                }
                let node = self.tree.get_mut(to);
                if node.status.is_running() {
                    node.status = NodeStatus::ended(&row.outcome);
                }
            }
            // The root's own refusals already reached its turn as tool
            // errors, and a row cannot say which turn it was.
            EdgeKind::Refusal if row.from != ROOT_NAME => {
                let Some(from) = self.node_named(&row.from) else {
                    return;
                };
                let root_task_id = self.tree.get(from).root_task_id.clone();
                let mut node = AgentNode::new(&row.to, &row.to, root_task_id.as_deref());
                node.status = NodeStatus::Refused {
                    reason: row.outcome.clone(),
                };
                let id = self.tree.push(from, node);
                let mut chain = self.chains.get(&from).cloned().unwrap_or_default();
                chain.push(row.to.clone());
                self.chains.insert(id, chain);
            }
            EdgeKind::Refusal | EdgeKind::Message => return,
        }
        self.revision += 1;
    }

    /// Give a node named by its spec the name the broker admitted it
    /// under.
    fn name(&mut self, id: NodeId, name: &str) {
        self.unnamed.retain(|n| *n != id);
        self.tree.get_mut(id).name = name.to_string();
    }

    /// Whether `id` is in `ancestor`'s subtree.
    fn is_below(&self, id: NodeId, ancestor: NodeId) -> bool {
        std::iter::successors(Some(id), |n| self.tree.parent(*n)).any(|n| n == ancestor)
    }

    /// The root's callee `id` runs under, if it is one of its runs.
    fn callee_above(&self, id: NodeId) -> Option<NodeId> {
        std::iter::successors(Some(id), |n| self.tree.parent(*n))
            .find(|n| self.callees.contains_key(n))
    }

    /// The node `batch` is about, made when this is its first.
    fn node_for(&mut self, batch: &SwarmEvent) -> NodeId {
        if let Some(id) = self.node_named(&batch.node) {
            return id;
        }
        let chain = &batch.chain.chain;
        let root_task_id = batch.root_task_id.as_str();
        // A child that reported first left a stand-in for this run.
        if let Some(id) = self.unnamed.iter().copied().find(|id| {
            !self.callees.contains_key(id)
                && self.chains.get(id) == Some(chain)
                && self.tree.get(*id).root_task_id.as_deref() == Some(root_task_id)
        }) {
            self.name(id, &batch.node);
            return id;
        }
        let parent = self.parent_for(root_task_id, chain);
        let spec = chain.last().map_or(batch.node.as_str(), String::as_str);
        let id = self.tree.push(
            parent,
            AgentNode::new(&batch.node, spec, Some(root_task_id)),
        );
        self.chains.insert(id, chain.clone());
        id
    }

    /// The node a run at `chain` hangs under: the run one link up the same
    /// chain under the same root call — the latest still running, when
    /// there are several — or a stand-in for it. The root's callee learns
    /// its root call from the first of its descendants to report.
    fn parent_for(&mut self, root_task_id: &str, chain: &[String]) -> NodeId {
        if chain.len() <= 1 {
            return self.tree.root();
        }
        let up = &chain[..chain.len() - 1];
        if up.len() == 1 {
            return self.tree.root();
        }
        let candidates: Vec<NodeId> = self
            .tree
            .preorder()
            .into_iter()
            .filter(|id| {
                let node = self.tree.get(*id);
                self.chains.get(id).map(Vec::as_slice) == Some(up)
                    && match node.root_task_id.as_deref() {
                        Some(theirs) => theirs == root_task_id,
                        None => self.callees.contains_key(id),
                    }
            })
            .collect();
        let running = candidates
            .iter()
            .rev()
            .find(|id| self.tree.get(**id).status.is_running());
        if let Some(id) = running.or(candidates.last()).copied() {
            let node = self.tree.get_mut(id);
            if node.root_task_id.is_none() {
                node.root_task_id = Some(root_task_id.to_string());
            }
            return id;
        }
        let grandparent = self.parent_for(root_task_id, up);
        let spec = up.last().map_or("", String::as_str);
        let id = self
            .tree
            .push(grandparent, AgentNode::new(spec, spec, Some(root_task_id)));
        self.chains.insert(id, up.to_vec());
        self.unnamed.push(id);
        id
    }

    /// Recompute the root's callee's own tool calls from its delegation's
    /// step lines. Those carry its descendants' steps too, rendered the
    /// same way (`name` on a start, `✓ name` / `✗ name` on a finish), and
    /// the broker forwards the descendants' calls whole: while the callee
    /// has an `invoke_agent` call open, a line the descendants account for
    /// is theirs; every other line is the callee's. A descendant's step
    /// reaches the root before its batch does, so it is the callee's until
    /// then. The callee's own calls carry no id, arguments or result across
    /// the hop: each is named by its step line's place.
    fn settle_steps(&mut self, id: NodeId) {
        let Some(steps) = self.callees.get(&id) else {
            return;
        };
        let mut theirs: BTreeMap<(char, &str), usize> = BTreeMap::new();
        for below in self
            .tree
            .preorder()
            .into_iter()
            .filter(|n| *n != id && self.is_below(*n, id))
        {
            for call in &self.tree.get(below).tool_calls {
                *theirs.entry(('>', call.name.as_str())).or_default() += 1;
                let finish = match call.outcome {
                    ToolOutcome::Running => continue,
                    ToolOutcome::Done { .. } => '\u{2713}',
                    ToolOutcome::Failed { .. } => '\u{2717}',
                };
                *theirs.entry((finish, call.name.as_str())).or_default() += 1;
            }
        }
        let mut own: Vec<ToolCall> = Vec::new();
        let delegating = |own: &[ToolCall]| {
            own.iter()
                .any(|c| c.name == INVOKE_AGENT && c.outcome == ToolOutcome::Running)
        };
        for (index, step) in steps.iter().enumerate() {
            if step.starts_with("error: ") {
                continue;
            }
            let (kind, name) = match step.split_once(' ') {
                Some(("\u{2713}", name)) => ('\u{2713}', name),
                Some(("\u{2717}", name)) => ('\u{2717}', name),
                _ => ('>', step.as_str()),
            };
            if delegating(&own)
                && let Some(count) = theirs.get_mut(&(kind, name))
                && *count > 0
            {
                *count -= 1;
                continue;
            }
            match kind {
                '>' => own.push(ToolCall {
                    id: format!("step-{index}"),
                    name: name.to_string(),
                    plugin: plugin_of(name),
                    arguments: None,
                    outcome: ToolOutcome::Running,
                }),
                finish => {
                    if let Some(call) = own
                        .iter_mut()
                        .find(|c| c.name == name && c.outcome == ToolOutcome::Running)
                    {
                        call.outcome = if finish == '\u{2713}' {
                            ToolOutcome::Done {
                                result: String::new(),
                            }
                        } else {
                            ToolOutcome::Failed {
                                error: String::new(),
                            }
                        };
                    }
                }
            }
        }
        self.tree.get_mut(id).tool_calls = own;
    }

    /// Recompute `id`'s own spend from what it and its children reported.
    /// The root's own spend is its turn's, so it has nothing to settle.
    fn settle_usage(&mut self, id: NodeId) {
        if id == self.tree.root() {
            return;
        }
        let Some(reported) = self.reported.get(&id) else {
            return;
        };
        let children: Vec<UsageLine> = self
            .tree
            .children(id)
            .iter()
            .filter_map(|child| self.reported.get(child))
            .flatten()
            .cloned()
            .collect();
        let own = subtract(reported, &children);
        let node = self.tree.get_mut(id);
        node.model = main_model(&own).or_else(|| main_model(reported));
        node.usage = own;
    }
}

/// The tool a callee delegates with; its descendants' steps only reach
/// the root while one of its calls to it is open.
const INVOKE_AGENT: &str = "invoke_agent";

/// Whether `node` is a name the broker admits `spec` under: `<spec>-<n>`.
fn is_node_of(node: &str, spec: &str) -> bool {
    node.strip_prefix(spec)
        .and_then(|rest| rest.strip_prefix('-'))
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::message_types::ToolSource;
    use chatty_fabric::CallChain;
    use serde_json::json;

    fn batch(node: &str, chain: &CallChain, inner: Vec<SwarmItem>) -> SessionEvent {
        SessionEvent::SwarmEvent(SwarmEvent {
            root_task_id: chain.root_task_id.clone(),
            node: node.to_string(),
            chain: chain.clone(),
            inner,
        })
    }

    fn delegation(progress: InvokeAgentProgress) -> SessionEvent {
        SessionEvent::Delegation(progress)
    }

    fn started(spec: &str) -> SessionEvent {
        delegation(InvokeAgentProgress::Started {
            agent_name: spec.into(),
            prompt: "go".into(),
            source: ToolSource::Local,
        })
    }

    fn step(line: &str) -> SessionEvent {
        delegation(InvokeAgentProgress::Step(line.into()))
    }

    fn line(model: &str, input: u32, output: u32) -> TokenUsage {
        TokenUsage {
            model: Some(ModelRef {
                provider: crate::settings::models::providers_store::ProviderType::Ollama,
                model_id: model.into(),
            }),
            ..TokenUsage::new(input, output)
        }
    }

    fn wire(lines: &[(&str, u32, u32)]) -> Value {
        let lines: Vec<Value> = lines
            .iter()
            .map(|(model, input, output)| {
                json!({ "model": { "provider": "ollama", "model_id": model },
                        "inputTokens": input, "outputTokens": output })
            })
            .collect();
        json!({ "lines": lines })
    }

    fn own_tokens(trace: &SwarmTrace, name: &str) -> u64 {
        let id = trace.node_named(name).expect(name);
        trace
            .tree()
            .get(id)
            .usage
            .iter()
            .map(UsageLine::tokens)
            .sum()
    }

    fn calls(trace: &SwarmTrace, name: &str) -> Vec<(String, ToolOutcome)> {
        let id = trace.node_named(name).expect(name);
        trace
            .tree()
            .get(id)
            .tool_calls
            .iter()
            .map(|c| (c.name.clone(), c.outcome.clone()))
            .collect()
    }

    fn done() -> ToolOutcome {
        ToolOutcome::Done {
            result: String::new(),
        }
    }

    /// Every run reports its whole subtree's usage (AGE-415); the tree
    /// bills each its own, whichever report arrives first, and a
    /// grandchild that reports before its parent's first batch hangs under
    /// the parent once that arrives. The nodes sum to the root's bill.
    #[test]
    fn a_sub_leader_is_not_billed_its_workers_tokens() {
        let top = CallChain::root("t-1").extend("top").unwrap();
        let mid = top.extend("mid").unwrap();
        let leaf = mid.extend("leaf").unwrap();
        let mut trace = SwarmTrace::new();
        trace.apply(&SessionEvent::TurnStarted);
        trace.apply(&started("top"));
        trace.apply(&batch(
            "leaf-0",
            &leaf,
            vec![
                SwarmItem::TurnStarted,
                SwarmItem::Usage {
                    usage: wire(&[("leaf-model", 10, 5)]),
                },
            ],
        ));
        trace.apply(&batch(
            "mid-0",
            &mid,
            vec![
                SwarmItem::TurnStarted,
                SwarmItem::Usage {
                    usage: wire(&[("mid-model", 100, 20), ("leaf-model", 10, 5)]),
                },
            ],
        ));
        trace.apply(&delegation(InvokeAgentProgress::Finished {
            success: true,
            result: None,
            usage: vec![
                line("top-model", 1000, 200),
                line("mid-model", 100, 20),
                line("leaf-model", 10, 5),
            ],
        }));
        trace.apply(&SessionEvent::TokenUsage(line("root-model", 7, 3)));

        let tree = trace.tree();
        let top_id = trace.node_named("top").unwrap();
        let mid_id = trace.node_named("mid-0").unwrap();
        let leaf_id = trace.node_named("leaf-0").unwrap();
        assert_eq!(tree.children(tree.root()), [top_id]);
        assert_eq!(tree.children(top_id), [mid_id]);
        assert_eq!(tree.children(mid_id), [leaf_id]);
        assert_eq!(tree.len(), 4, "the stand-in became mid-0");
        assert_eq!(tree.get(top_id).root_task_id.as_deref(), Some("t-1"));
        assert_eq!(own_tokens(&trace, "top"), 1200);
        assert_eq!(own_tokens(&trace, "mid-0"), 120);
        assert_eq!(own_tokens(&trace, "leaf-0"), 15);
        assert_eq!(
            tree.get(mid_id).model.as_ref().unwrap().model_id,
            "mid-model"
        );
        let tokens = |lines: Vec<UsageLine>| lines.iter().map(UsageLine::tokens).sum::<u64>();
        assert_eq!(tokens(trace.total()), 1200 + 120 + 15 + 10);
        assert_eq!(tokens(trace.total()), tokens(trace.billed()));
    }

    /// The root's callee reports through its delegation, whose steps mix
    /// in its descendants': once their batches arrive, their lines are
    /// theirs and the rest are the callee's, in order.
    #[test]
    fn a_callees_steps_are_its_own_less_its_descendants() {
        let coder = CallChain::root("t-1")
            .extend("reviewer")
            .unwrap()
            .extend("coder")
            .unwrap();
        let mut trace = SwarmTrace::new();
        trace.apply(&started("reviewer"));
        for line in [
            "read_file",
            "\u{2713} read_file",
            "invoke_agent",
            "read_file",
        ] {
            trace.apply(&step(line));
        }
        // Live: the coder's step is the reviewer's until its batch comes.
        assert_eq!(calls(&trace, "reviewer").len(), 3);
        trace.apply(&batch(
            "coder-0",
            &coder,
            vec![
                SwarmItem::ToolCallStarted {
                    id: "c1".into(),
                    name: "read_file".into(),
                },
                SwarmItem::ToolCallError {
                    id: "c1".into(),
                    error: "no such file".into(),
                },
            ],
        ));
        for line in ["\u{2717} read_file", "\u{2713} invoke_agent"] {
            trace.apply(&step(line));
        }
        assert_eq!(
            calls(&trace, "reviewer"),
            [
                ("read_file".into(), done()),
                ("invoke_agent".into(), done())
            ]
        );
        assert_eq!(calls(&trace, "coder-0").len(), 1);
    }

    /// Two runs of one spec under one parent: the edge log says which of
    /// them a grandchild's call came from, names the root's callee, and
    /// adds the calls that were refused.
    #[test]
    fn the_edge_log_settles_which_sibling_called() {
        let top = CallChain::root("t-1").extend("top").unwrap();
        let mid = top.extend("mid").unwrap();
        let leaf = mid.extend("leaf").unwrap();
        let mut trace = SwarmTrace::new();
        trace.apply(&started("top"));
        for node in ["mid-0", "mid-1"] {
            trace.apply(&batch(node, &mid, vec![SwarmItem::TurnStarted]));
        }
        trace.apply(&batch("leaf-0", &leaf, vec![SwarmItem::TurnStarted]));
        let row = |kind, from: &str, to: &str, outcome: &str| EdgeRow {
            ts: 0,
            kind,
            from: from.into(),
            to: to.into(),
            scope: None,
            run: None,
            chain: vec![],
            bytes: 0,
            outcome: outcome.into(),
            usd: None,
        };
        let revision = trace.revision();
        for r in [
            row(EdgeKind::Task, "mid-0", "leaf-0", "completed"),
            row(EdgeKind::Refusal, "mid-1", "top", "cycle"),
            row(EdgeKind::Task, "top-0", "mid-0", "completed"),
            row(EdgeKind::Task, "root", "top-0", "completed"),
            // Another turn's rows name nobody here.
            row(EdgeKind::Task, "root", "other-0", "completed"),
            row(EdgeKind::Task, "other-0", "x-0", "completed"),
        ] {
            trace.apply_edge(&r);
        }
        assert!(trace.revision() > revision);

        let tree = trace.tree();
        let top0 = trace.node_named("top-0").expect("the callee is named");
        let mid0 = trace.node_named("mid-0").unwrap();
        let mid1 = trace.node_named("mid-1").unwrap();
        let leaf0 = trace.node_named("leaf-0").unwrap();
        assert_eq!(tree.children(tree.root()), [top0]);
        assert_eq!(tree.parent(leaf0), Some(mid0));
        assert_eq!(tree.get(leaf0).status, NodeStatus::Completed);
        assert_eq!(tree.get(top0).status, NodeStatus::Completed);
        let refused = tree.children(mid1);
        assert_eq!(refused.len(), 1);
        assert_eq!(
            tree.get(refused[0]).status,
            NodeStatus::Refused {
                reason: "cycle".into()
            }
        );
        assert_eq!(tree.len(), 6);
    }

    #[test]
    fn plugin_tools_name_their_plugin() {
        assert_eq!(plugin_of("benford__analyze").as_deref(), Some("benford"));
        assert_eq!(plugin_of("read_file"), None);
        assert_eq!(plugin_of("__x"), None);
    }

    #[test]
    fn node_names_are_spec_dash_number() {
        assert!(is_node_of("kit-coder-0", "kit-coder"));
        assert!(is_node_of("kit-coder-12", "kit-coder"));
        assert!(!is_node_of("kit-coder-x", "kit-coder"));
        assert!(!is_node_of("kit-coder-", "kit-coder"));
        assert!(!is_node_of("kit-coderz-0", "kit-coder"));
    }
}
