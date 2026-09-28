//! Who is registered, and where each open task's updates go.
//!
//! The registry is the broker's memory for the process lifetime: ADR-0011
//! puts discovery, liveness and accounting in one place, and this is that
//! place for local participants. Nothing here is persisted — a participant is
//! a live connection, so an entry that outlived its socket would be a lie.
//!
//! # Names (ADR-0020)
//!
//! A participant does not choose its name. The broker admits a node into its
//! [`Directory`] — which names it `<spec>-<n>`, never reusing a name — makes
//! the connection for it, and registers that connection under the admitted
//! name when the worker says `hello`. Nothing else can register: there is no
//! entry point that takes a name from the caller, so no process can squat on
//! a name the broker is about to route a task to.
//!
//! # Locking
//!
//! One `Mutex` over the whole map, held only for map surgery — never across
//! an await. The traffic is one small frame per progress event on a
//! single-digit number of participants; a finer-grained scheme would buy
//! nothing and cost the invariant that a deregistration cannot interleave
//! with a task submission.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use serde_json::Value;

use super::calls::{BrokerCalls, Caller};
use super::protocol::{
    BrokerFrame, DelegatedTask, InputRequest, ParticipantCard, ParticipantFrame, TaskInput,
    TaskState,
};
use chatty_fabric::{
    AgentOrigin, CallChain, ConversationScope, Directory, DirectoryError, Node, NodeId, NodeName,
    NodeState, RunId, RunPermit, SpawnContext, SwarmItem, TaskTable, WeakRunPermit,
};

/// The conversation scope every node this broker admits works for, until
/// the spawn request carries the caller's own scope.
pub const ROOT_SCOPE: &str = "root";

/// One update on an open task, as the HTTP side consumes it.
///
/// This is the A2A event model minus its JSON-RPC envelope: the handler adds
/// the envelope because only it knows the request's `id`.
#[derive(Debug, Clone)]
pub enum TaskUpdate {
    Status {
        state: TaskState,
        message: Option<String>,
        /// Opaque, forwarded to the A2A status's `metadata` (see
        /// [`ParticipantFrame::Status`](super::protocol::ParticipantFrame)).
        metadata: Option<Value>,
        /// What an `input-required` task is waiting for; answered through
        /// [`ParticipantRegistry::answer_task`].
        input: Option<InputRequest>,
    },
    Artifact {
        text: String,
        last_chunk: bool,
    },
    /// One of the worker's own turns or tool events, on a task sent with
    /// `swarmEvents` (TB-1). Untagged: the broker's call path tags it.
    Event(SwarmItem),
}

/// The stream of updates for one submitted task.
///
/// Ends when a terminal status arrives, or when the participant's socket
/// closes — [`ParticipantRegistry::deregister`] pushes a `failed` status
/// before dropping the sender, so a caller is never left waiting on a
/// process that has gone away.
pub type TaskStream = mpsc::UnboundedReceiver<TaskUpdate>;

/// Why an answer could not be delivered.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AnswerError {
    #[error("task '{0}' is not open on any participant")]
    UnknownTask(String),
    #[error("the participant owning task '{0}' is disconnecting")]
    ParticipantGone(String),
}

/// A node the broker admitted for a connection it is making.
///
/// Held by whoever serves that connection and consumed when the worker's
/// `hello` registers it ([`ParticipantRegistry::register`]) or when the
/// connection closes first ([`ParticipantRegistry::abandon`]). Not `Clone`:
/// one admission is one connection.
#[derive(Debug)]
pub struct AdmittedNode {
    id: NodeId,
    name: NodeName,
    scope: ConversationScope,
    owner: Option<NodeName>,
    origin: AgentOrigin,
}

impl AdmittedNode {
    /// The name callers will address the node by.
    pub fn name(&self) -> &str {
        self.name.as_str()
    }

    /// The `welcome` frame that tells the worker who it is.
    pub fn welcome(&self) -> BrokerFrame {
        BrokerFrame::Welcome {
            name: self.name.clone(),
            scope: self.scope.clone(),
            owner: self.owner.clone(),
        }
    }
}

/// A registered agent as the broker serves it: what it says about itself,
/// plus where the broker knows it came from (ADR-0011 C5).
#[derive(Debug, Clone)]
pub struct RegisteredAgent {
    pub card: ParticipantCard,
    pub origin: AgentOrigin,
}

struct Participant {
    /// The node this connection was admitted as.
    node: NodeId,
    card: ParticipantCard,
    /// Where this registration arrived from. The participant does not get a
    /// say — see [`AgentOrigin`].
    origin: AgentOrigin,
    /// Frames queued for this participant's socket writer.
    outbound: mpsc::UnboundedSender<BrokerFrame>,
    /// Open tasks: id → where this task's updates go.
    tasks: HashMap<String, mpsc::UnboundedSender<TaskUpdate>>,
}

#[derive(Default)]
struct Inner {
    participants: HashMap<String, Participant>,
    /// Every node this broker admitted, connected or not, ended ones
    /// included — which is what keeps a name from being issued twice.
    directory: Directory,
    /// What runs the `call` frames a connection sends (BI-4). Weak: the
    /// gateway serving this registry owns it, and a connection outliving
    /// the gateway has nothing left to call.
    calls: Weak<BrokerCalls>,
    /// Each spawned node's own context, which its calls spawn from (BI-5):
    /// its own tree and branch, and the roster it was given. Kept while
    /// the node's connection is open.
    contexts: HashMap<String, SpawnContext>,
    /// Each metered node's endpoint permit (BI-6): released while the
    /// node's calls are outstanding, re-acquired before the last result is
    /// delivered. Weak: the node's worker owns it, so reaping the worker
    /// frees the slot at once, not when its connection is next read.
    permits: HashMap<String, WeakRunPermit>,
    /// Where a copy of every line a connection reads or writes goes, when
    /// someone asked for one ([`ParticipantRegistry::tap_wire`]).
    wire_tap: Option<mpsc::UnboundedSender<String>>,
    /// The runs broker calls started, each with the chain the broker
    /// stamped on it (DP-2). A node's calls extend the chain of the run it
    /// serves.
    runs: TaskTable,
}

/// A run in the broker's task table, released when this is dropped: when
/// the worker serving it is reaped, however the call ended.
#[must_use = "dropping the guard releases the run"]
pub struct RunGuard {
    registry: ParticipantRegistry,
    run: RunId,
}

impl RunGuard {
    pub fn run(&self) -> RunId {
        self.run
    }
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        self.registry.lock().runs.release(self.run);
    }
}

impl std::fmt::Debug for RunGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("RunGuard").field(&self.run).finish()
    }
}

/// The broker's live local participants. Cheap to clone; all clones share
/// one map.
#[derive(Clone, Default)]
pub struct ParticipantRegistry {
    inner: Arc<Mutex<Inner>>,
    next_task: Arc<AtomicU64>,
}

impl ParticipantRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Run the `call` frames this registry's connections send on `calls`
    /// (BI-4). The gateway does this once it knows its virtual agents.
    pub fn install_calls(&self, calls: &Arc<BrokerCalls>) {
        self.lock().calls = Arc::downgrade(calls);
    }

    /// A copy of every frame line the connections opened from now on read
    /// (`<- …`) and write (`-> …`), exactly as it crossed the socket. What
    /// a test compares when it says a change leaves the wire byte-identical
    /// (TD-2, AGE-693); nothing in the broker reads it.
    pub fn tap_wire(&self) -> mpsc::UnboundedReceiver<String> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.lock().wire_tap = Some(tx);
        rx
    }

    /// The tap [`tap_wire`](Self::tap_wire) installed, if any.
    #[cfg_attr(not(unix), allow(dead_code))]
    pub(crate) fn wire_tap(&self) -> Option<mpsc::UnboundedSender<String>> {
        self.lock().wire_tap.clone()
    }

    /// What runs a connection's calls, while the gateway that installed it
    /// is alive.
    pub(crate) fn calls(&self) -> Option<Arc<BrokerCalls>> {
        self.lock().calls.upgrade()
    }

    /// Admit a node started as `spec` and name it, before its connection
    /// exists. `origin` is the transport's, not the participant's: a
    /// socket pair on this machine is [`AgentOrigin::Local`], a leased
    /// microVM's vsock [`AgentOrigin::Fleet`].
    ///
    /// `owner` is the node whose call spawns this one — the calling node the
    /// broker stamped on the task (DP-2) — or `None` when the root asked.
    /// The directory records it, so the new node's tree is the real one:
    /// its `welcome` names it, its `send_message` may address only it, and
    /// its ancestors run through it (TM-2b). A name no node was admitted
    /// under is refused, never quietly made the root's.
    pub fn admit(
        &self,
        spec: &str,
        origin: AgentOrigin,
        owner: Option<&str>,
    ) -> Result<AdmittedNode, DirectoryError> {
        let scope = ConversationScope::new(ROOT_SCOPE);
        let mut inner = self.lock();
        let owner = owner
            .map(|name| {
                inner
                    .directory
                    .by_name(name)
                    .map(|node| (node.id(), node.name().clone()))
                    .ok_or_else(|| DirectoryError::UnknownOwnerName(name.to_string()))
            })
            .transpose()?;
        let node = inner
            .directory
            .admit(spec, owner.as_ref().map(|(id, _)| *id), scope)?;
        drop(inner);
        let owner = owner.map(|(_, name)| name);
        debug!(node = %node.name(), spec, ?owner, "Admitted a node");
        Ok(AdmittedNode {
            id: node.id(),
            name: node.name().clone(),
            scope: node.scope().clone(),
            owner,
            origin,
        })
    }

    /// Record `name`'s own context: the tree it works in, the branch it
    /// works on and the roster it was given (BI-5). The runner that spawned
    /// it calls this once its tree exists; a call the node makes spawns
    /// from here, clamped to it. Forgotten when the node's connection
    /// closes.
    pub fn set_node_context(&self, name: &str, context: SpawnContext) {
        self.lock().contexts.insert(name.to_string(), context);
    }

    /// `name`'s own context, if a runner recorded one (BI-5).
    pub fn node_context(&self, name: &str) -> Option<SpawnContext> {
        self.lock().contexts.get(name).cloned()
    }

    /// Open the run a broker call started, served by the node admitted as
    /// `callee` and called by `caller` (`None`: the root), under `chain`
    /// (DP-2). A runner does this once it has admitted the node and before
    /// the worker exists, so every call the worker makes finds its chain.
    /// `None` when `callee` was never admitted.
    pub fn open_run(
        &self,
        callee: &str,
        caller: Option<&str>,
        chain: CallChain,
    ) -> Option<RunGuard> {
        let mut inner = self.lock();
        let callee = inner.directory.by_name(callee)?.id();
        let caller = caller.and_then(|name| inner.directory.by_name(name).map(Node::id));
        let parent = caller.and_then(|node| inner.runs.served_by(node).map(|(run, _)| run).last());
        let run = inner.runs.open(caller, callee, parent, chain).ok()?;
        drop(inner);
        Some(RunGuard {
            registry: self.clone(),
            run,
        })
    }

    /// The chain of the run `name` serves (DP-2): the broker's own record,
    /// never anything the node said. `None` for a node no broker call
    /// started.
    pub(crate) fn run_chain(&self, name: &str) -> Option<CallChain> {
        let inner = self.lock();
        let node = inner.directory.by_name(name)?.id();
        inner
            .runs
            .served_by(node)
            .map(|(_, entry)| entry.chain.clone())
            .max_by_key(|chain| chain.depth)
    }

    /// The spec `name` was admitted as.
    pub(crate) fn node_spec(&self, name: &str) -> Option<String> {
        self.lock()
            .directory
            .by_name(name)
            .map(|node| node.spec().to_string())
    }

    /// Every node this broker ever admitted, ended ones included, in
    /// admission order. A runner admits a node for every worker process it
    /// starts, so this is also every worker it spawned.
    pub fn admitted(&self) -> Vec<String> {
        let inner = self.lock();
        let scope = ConversationScope::new(ROOT_SCOPE);
        inner
            .directory
            .in_scope(&scope)
            .map(|node| node.name().as_str().to_string())
            .collect()
    }

    /// How many runs are open (DP-2 tests).
    pub fn open_runs(&self) -> usize {
        self.lock().runs.len()
    }

    /// Record the endpoint permit `name`'s run holds (BI-6), without
    /// keeping it: the runner that spawned the node owns it. A call the node
    /// makes releases it until the call's result is due. Forgotten when the
    /// node's connection closes.
    pub fn set_node_permit(&self, name: &str, permit: &RunPermit) {
        self.lock()
            .permits
            .insert(name.to_string(), permit.downgrade());
    }

    /// `name`'s endpoint permit, while its runner meters a live run (BI-6).
    pub fn node_permit(&self, name: &str) -> Option<RunPermit> {
        self.lock()
            .permits
            .get(name)
            .and_then(WeakRunPermit::upgrade)
    }

    /// Register `node`'s connection: its worker said `hello` with `card`,
    /// and frames for it go to `outbound`. Returns the name it is served
    /// under, which is the admitted one whatever the card says.
    ///
    /// Crate-private on purpose: the only caller is the connection loop, so
    /// nothing can register a name without the broker having made the
    /// connection for it.
    #[cfg_attr(not(unix), allow(dead_code))]
    pub(crate) fn register(
        &self,
        node: AdmittedNode,
        card: ParticipantCard,
        outbound: mpsc::UnboundedSender<BrokerFrame>,
    ) -> String {
        let name = node.name.as_str().to_string();
        if card.name != name && !card.name.is_empty() {
            debug!(participant = %name, claimed = %card.name, "Ignoring the name a card claims");
        }
        let mut inner = self.lock();
        let _ = inner.directory.set_state(node.id, NodeState::Idle);
        inner.participants.insert(
            name.clone(),
            Participant {
                node: node.id,
                card: ParticipantCard {
                    name: name.clone(),
                    ..card
                },
                origin: node.origin,
                outbound,
                tasks: HashMap::new(),
            },
        );
        drop(inner);

        info!(participant = %name, origin = %node.origin, "Participant registered");
        name
    }

    /// `node`'s connection closed before it registered. Its name stays
    /// spent.
    pub fn abandon(&self, node: AdmittedNode) {
        let mut inner = self.lock();
        let _ = inner.directory.end(node.id);
        inner.contexts.remove(node.name.as_str());
        inner.permits.remove(node.name.as_str());
        drop(inner);
        self.ended(node.id, node.name.as_str());
        debug!(node = %node.name, "A node's connection closed before it said hello");
    }

    /// The node `id` ended: the messages waiting for it die with it (tree
    /// messages, TM-2). Called with the lock released, since the call path
    /// reads the directory.
    fn ended(&self, id: NodeId, name: &str) {
        if let Some(calls) = self.calls() {
            calls.recipient_ended(id, name);
        }
    }

    /// Drop a participant and fail everything it still owed.
    ///
    /// Called when the socket closes, however it closed — a clean exit and a
    /// crash are the same event from here, which is the point: liveness is
    /// the connection, not a heartbeat the participant could lie about.
    pub fn deregister(&self, name: &str) {
        let participant = {
            let mut inner = self.lock();
            inner.contexts.remove(name);
            inner.permits.remove(name);
            let Some(participant) = inner.participants.remove(name) else {
                return;
            };
            let _ = inner.directory.end(participant.node);
            participant
        };
        self.ended(participant.node, name);

        let open = participant.tasks.len();
        for (task_id, sink) in participant.tasks {
            debug!(participant = %name, task = %task_id, "Failing a task whose participant went away");
            let _ = sink.send(TaskUpdate::Status {
                state: TaskState::Failed,
                message: Some(format!("participant '{name}' disconnected")),
                metadata: None,
                input: None,
            });
        }

        info!(participant = %name, failed_tasks = open, "Local participant deregistered");
    }

    /// Names of every registered participant, sorted so listings are stable.
    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.lock().participants.keys().cloned().collect();
        names.sort();
        names
    }

    pub fn is_registered(&self, name: &str) -> bool {
        self.lock().participants.contains_key(name)
    }

    /// The node admitted as `name`, ended or not, and its owner's node —
    /// `None` for the owner when the root owns it. `None` when no node was
    /// ever admitted under that name.
    pub(crate) fn node_and_owner(&self, name: &str) -> Option<(Node, Option<Node>)> {
        let inner = self.lock();
        let node = inner.directory.by_name(name)?.clone();
        let owner = node
            .owner()
            .and_then(|owner| inner.directory.get(owner).cloned());
        Some((node, owner))
    }

    pub fn card(&self, name: &str) -> Option<ParticipantCard> {
        self.lock().participants.get(name).map(|p| p.card.clone())
    }

    /// Where `name` came from, if it is registered.
    pub fn origin(&self, name: &str) -> Option<AgentOrigin> {
        self.lock().participants.get(name).map(|p| p.origin)
    }

    /// Every registered agent, in [`names`](Self::names) order.
    pub fn agents(&self) -> Vec<RegisteredAgent> {
        let inner = self.lock();
        let mut agents: Vec<RegisteredAgent> = inner
            .participants
            .values()
            .map(|p| RegisteredAgent {
                card: p.card.clone(),
                origin: p.origin,
            })
            .collect();
        agents.sort_by(|a, b| a.card.name.cmp(&b.card.name));
        agents
    }

    /// Hand `task` to `name` as a new task.
    ///
    /// Returns the task's id and its update stream, or `None` if the
    /// participant is not registered or its socket writer has already gone.
    ///
    /// A new task is the participant's next run: the messages waiting for
    /// it open the task's text, and each sender's allowance starts over
    /// (tree messages, TM-2).
    pub fn submit_task(&self, name: &str, mut task: DelegatedTask) -> Option<(String, TaskStream)> {
        if !self.is_registered(name) {
            return None;
        }
        let messages = self
            .calls()
            .map(|calls| calls.start_run(&Caller::Node(name.to_string())))
            .unwrap_or_default();
        if !messages.is_empty() {
            task.text = format!("{}\n\n{}", messages.join("\n"), task.text);
        }
        let task_id = format!(
            "task-{}-{}",
            crate::gateway::new_id(),
            self.next_task.fetch_add(1, Ordering::Relaxed)
        );
        let (tx, rx) = mpsc::unbounded_channel();

        let mut inner = self.lock();
        let participant = inner.participants.get_mut(name)?;
        if participant
            .outbound
            .send(BrokerFrame::Task {
                task_id: task_id.clone(),
                budget: Box::new(task.frame_budget()),
                text: task.text,
                bearer: task.bearer,
                capture_conversation: task.capture_conversation,
                spawn_context: task.spawn_context,
                handoff: task.handoff,
                swarm_events: task.swarm_events,
            })
            .is_err()
        {
            // The writer task is gone; the read loop's deregister is on its
            // way. Don't hand the caller a stream nothing will ever feed.
            warn!(participant = %name, "Dropping a task: the participant's socket is closing");
            return None;
        }
        participant.tasks.insert(task_id.clone(), tx);
        drop(inner);

        debug!(participant = %name, task = %task_id, "Task submitted to a local participant");
        Some((task_id, rx))
    }

    /// Tell a participant to stop, and forget the task.
    ///
    /// Used when a caller hangs up mid-task. No reply is expected: the
    /// participant's own terminal status, if it sends one, lands on a task
    /// that is already gone and is dropped by [`on_frame`](Self::on_frame).
    pub fn cancel_task(&self, name: &str, task_id: &str) {
        let mut inner = self.lock();
        let Some(participant) = inner.participants.get_mut(name) else {
            return;
        };
        if participant.tasks.remove(task_id).is_some() {
            let _ = participant.outbound.send(BrokerFrame::Cancel {
                task_id: task_id.to_string(),
            });
            debug!(participant = %name, task = %task_id, "Task cancelled");
        }
    }

    /// Whether `task_id` is open on some participant.
    ///
    /// The A2A handler asks this to tell a `message/send` that answers a
    /// parked task from one that starts a new task: only an id the broker
    /// itself minted, and has not closed, can be the former.
    pub fn owns_task(&self, task_id: &str) -> bool {
        self.lock()
            .participants
            .values()
            .any(|p| p.tasks.contains_key(task_id))
    }

    /// Deliver the answer to a task parked in `input-required`.
    ///
    /// Addressed by task id alone: the caller answered on the A2A task it
    /// was streaming, and a runner's task is served by a worker whose
    /// participant name the caller never learned. The task stays open — the
    /// participant's next status un-parks it.
    pub fn answer_task(&self, task_id: &str, input: TaskInput) -> Result<(), AnswerError> {
        let mut inner = self.lock();
        let Some((name, participant)) = inner
            .participants
            .iter_mut()
            .find(|(_, p)| p.tasks.contains_key(task_id))
        else {
            return Err(AnswerError::UnknownTask(task_id.to_string()));
        };
        let name = name.clone();
        if participant
            .outbound
            .send(BrokerFrame::Input {
                task_id: task_id.to_string(),
                input,
            })
            .is_err()
        {
            return Err(AnswerError::ParticipantGone(task_id.to_string()));
        }
        drop(inner);
        debug!(participant = %name, task = %task_id, "Answer delivered to a parked task");
        Ok(())
    }

    /// Route one frame from `name`'s socket to the task it names.
    ///
    /// A frame for an unknown task is dropped with a log line rather than
    /// killing the connection: the caller may simply have hung up first, and
    /// a participant is not required to notice before its next frame.
    ///
    /// [`ParticipantFrame::Hello`] is handled by the connection loop
    /// before any task traffic; a second one arriving here is a protocol
    /// error and is reported as `false`.
    pub fn on_frame(&self, name: &str, frame: ParticipantFrame) -> bool {
        let (task_id, update, terminal) = match frame {
            ParticipantFrame::Hello { .. } => {
                warn!(participant = %name, "A second hello on one connection");
                return false;
            }
            ParticipantFrame::Status {
                task_id,
                state,
                message,
                metadata,
                input,
            } => (
                task_id,
                TaskUpdate::Status {
                    state,
                    message,
                    metadata,
                    input,
                },
                state.is_terminal(),
            ),
            ParticipantFrame::Artifact {
                task_id,
                text,
                last_chunk,
            } => (task_id, TaskUpdate::Artifact { text, last_chunk }, false),
            ParticipantFrame::Event { task_id, event } => {
                (task_id, TaskUpdate::Event(event), false)
            }
            // Calls are run by the connection loop, which owns their
            // lifetime; one reaching here was not routed and is dropped.
            ParticipantFrame::Call { id, .. } | ParticipantFrame::CallInput { id, .. } => {
                warn!(participant = %name, call = id, "A call frame outside a connection loop");
                return true;
            }
        };

        let mut inner = self.lock();
        let Some(participant) = inner.participants.get_mut(name) else {
            return true;
        };
        // A terminal status is the task's last update, so the entry is taken
        // out before the send: the sink is dropped with this scope, which is
        // what ends the caller's stream.
        let sink = if terminal {
            participant.tasks.remove(&task_id)
        } else {
            participant.tasks.get(&task_id).cloned()
        };
        drop(inner);

        match sink {
            Some(sink) => {
                let _ = sink.send(update);
            }
            None => debug!(
                participant = %name,
                task = %task_id,
                "Dropping an update for a task nobody is waiting on"
            ),
        }
        true
    }

    /// Open task count for `name` — liveness assertions in tests, and the
    /// per-endpoint budget AGE-305 will meter.
    pub fn open_task_count(&self, name: &str) -> usize {
        self.lock()
            .participants
            .get(name)
            .map(|p| p.tasks.len())
            .unwrap_or(0)
    }

    /// A poisoned lock means a panic happened while the map was being
    /// edited. The map is a plain `HashMap` of owned values with no
    /// cross-entry invariant, so recovering it is sound and strictly better
    /// than taking the whole broker down with every participant on it.
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
impl ParticipantRegistry {
    /// Admit a node as `spec` owned by the node named `owner`, or by the
    /// root when `None`, as a spawn does, and return its name.
    pub(crate) fn admit_under(&self, spec: &str, owner: Option<&str>) -> String {
        self.admit(spec, AgentOrigin::Local, owner)
            .expect("admitted")
            .name()
            .to_string()
    }

    /// Mark the node named `name` ended, as its connection closing does.
    pub(crate) fn end_node(&self, name: &str) {
        let mut inner = self.lock();
        let id = inner.directory.by_name(name).expect("the node").id();
        inner.directory.end(id).expect("ended");
        drop(inner);
        self.ended(id, name);
    }
}

#[cfg(test)]
mod tests {
    use super::super::protocol::TaskBearer;
    use super::*;

    fn card(name: &str) -> ParticipantCard {
        ParticipantCard {
            name: name.to_string(),
            description: "a test participant".to_string(),
            ..Default::default()
        }
    }

    /// Admit a node as `spec`, register it, and return its name with the
    /// outbound receiver kept alive for the caller.
    fn register(
        reg: &ParticipantRegistry,
        spec: &str,
    ) -> (String, mpsc::UnboundedReceiver<BrokerFrame>) {
        register_from(reg, spec, AgentOrigin::Local)
    }

    fn register_from(
        reg: &ParticipantRegistry,
        spec: &str,
        origin: AgentOrigin,
    ) -> (String, mpsc::UnboundedReceiver<BrokerFrame>) {
        let node = reg
            .admit(spec, origin, None)
            .expect("a root node is admitted");
        let (tx, rx) = mpsc::unbounded_channel();
        (reg.register(node, card(spec), tx), rx)
    }

    #[test]
    fn a_registered_participant_is_addressable_by_its_admitted_name() {
        let reg = ParticipantRegistry::new();
        let (w, _outbound) = register(&reg, "worker");

        assert_eq!(w, "worker-0", "the broker names the node <spec>-<n>");
        assert!(reg.is_registered(&w));
        assert_eq!(reg.names(), vec![w.clone()]);
        assert_eq!(reg.card(&w).unwrap().description, "a test participant");
        assert!(reg.card("nobody").is_none());
    }

    #[test]
    fn the_cards_name_is_replaced_by_the_admitted_one() {
        let reg = ParticipantRegistry::new();
        let node = reg.admit("local-coder", AgentOrigin::Local, None).unwrap();
        let (tx, _rx) = mpsc::unbounded_channel();
        let name = reg.register(node, card("evil"), tx);

        assert_eq!(name, "local-coder-0");
        assert_eq!(reg.card(&name).unwrap().name, "local-coder-0");
        assert!(!reg.is_registered("evil"));
    }

    #[test]
    fn names_are_never_issued_twice_even_after_a_node_ends() {
        let reg = ParticipantRegistry::new();
        let (first, _outbound) = register(&reg, "worker");
        reg.deregister(&first);
        let abandoned = reg.admit("worker", AgentOrigin::Local, None).unwrap();
        assert_eq!(abandoned.name(), "worker-1");
        reg.abandon(abandoned);

        let (third, _outbound) = register(&reg, "worker");
        assert_eq!(third, "worker-2");
    }

    /// A spawn records its caller as the new node's owner, and the welcome
    /// names it; a caller no node was admitted under is refused rather
    /// than made the root.
    #[test]
    fn a_spawn_is_owned_by_its_caller() {
        let reg = ParticipantRegistry::new();
        let lead = reg.admit("lead", AgentOrigin::Local, None).unwrap();
        let worker = reg
            .admit("worker", AgentOrigin::Local, Some(lead.name()))
            .unwrap();
        let BrokerFrame::Welcome { owner, .. } = worker.welcome() else {
            panic!("a welcome");
        };
        assert_eq!(owner.as_ref().map(|o| o.as_str()), Some("lead-0"));
        let (_, owner) = reg.node_and_owner(worker.name()).unwrap();
        assert_eq!(owner.unwrap().name().as_str(), "lead-0");

        assert_eq!(
            reg.admit("worker", AgentOrigin::Local, Some("nobody-0"))
                .err(),
            Some(DirectoryError::UnknownOwnerName("nobody-0".to_string()))
        );
    }

    #[tokio::test]
    async fn a_task_reaches_the_participant_and_its_updates_come_back() {
        let reg = ParticipantRegistry::new();
        let (w, mut outbound) = register(&reg, "worker");

        let (task_id, mut updates) = reg
            .submit_task(
                &w,
                DelegatedTask::new("summarise foo.rs").with_bearer(Some(TaskBearer::new("tok"))),
            )
            .expect("the participant is registered");

        let BrokerFrame::Task {
            task_id: sent,
            text,
            bearer,
            capture_conversation,
            ..
        } = outbound.recv().await.unwrap()
        else {
            panic!("expected a task frame");
        };
        assert_eq!(sent, task_id);
        assert_eq!(text, "summarise foo.rs");
        // The caller's bearer rides the frame to the worker (AGE-371).
        assert_eq!(bearer, Some(TaskBearer::new("tok")));
        assert!(!capture_conversation, "not asked for, so off by default");
        assert_eq!(reg.open_task_count(&w), 1);

        reg.on_frame(
            &w,
            ParticipantFrame::Status {
                task_id: task_id.clone(),
                state: TaskState::Working,
                message: Some("read_file".into()),
                metadata: None,
                input: None,
            },
        );
        reg.on_frame(
            &w,
            ParticipantFrame::Artifact {
                task_id: task_id.clone(),
                text: "foo.rs defines Foo".into(),
                last_chunk: true,
            },
        );
        reg.on_frame(
            &w,
            ParticipantFrame::Status {
                task_id: task_id.clone(),
                state: TaskState::Completed,
                message: None,
                metadata: None,
                input: None,
            },
        );

        assert!(matches!(
            updates.recv().await,
            Some(TaskUpdate::Status { state: TaskState::Working, message: Some(m), .. }) if m == "read_file"
        ));
        assert!(matches!(
            updates.recv().await,
            Some(TaskUpdate::Artifact { text, last_chunk: true }) if text == "foo.rs defines Foo"
        ));
        assert!(matches!(
            updates.recv().await,
            Some(TaskUpdate::Status {
                state: TaskState::Completed,
                ..
            })
        ));
        assert!(
            updates.recv().await.is_none(),
            "a terminal status ends the stream"
        );
        assert_eq!(
            reg.open_task_count(&w),
            0,
            "a completed task is no longer open"
        );
    }

    #[tokio::test]
    async fn a_disconnect_fails_every_open_task() {
        let reg = ParticipantRegistry::new();
        let (w, _outbound) = register(&reg, "worker");

        let (_a, mut first) = reg.submit_task(&w, DelegatedTask::new("a")).unwrap();
        let (_b, mut second) = reg.submit_task(&w, DelegatedTask::new("b")).unwrap();

        reg.deregister(&w);

        for stream in [&mut first, &mut second] {
            let update = stream
                .recv()
                .await
                .expect("an open task is told why it died");
            assert!(matches!(
                update,
                TaskUpdate::Status { state: TaskState::Failed, message: Some(ref m), .. }
                    if m.contains("disconnected")
            ));
            assert!(stream.recv().await.is_none(), "then the stream ends");
        }
        assert!(!reg.is_registered(&w));
        assert!(reg.names().is_empty());
    }

    #[test]
    fn a_task_for_an_unregistered_participant_is_not_accepted() {
        let reg = ParticipantRegistry::new();
        assert!(
            reg.submit_task("nobody", DelegatedTask::new("hello"))
                .is_none()
        );
    }

    #[tokio::test]
    async fn cancelling_forgets_the_task_and_tells_the_participant() {
        let reg = ParticipantRegistry::new();
        let (w, mut outbound) = register(&reg, "worker");
        let (task_id, mut updates) = reg.submit_task(&w, DelegatedTask::new("a")).unwrap();
        let _ = outbound.recv().await;

        reg.cancel_task(&w, &task_id);

        assert!(matches!(
            outbound.recv().await,
            Some(BrokerFrame::Cancel { task_id: t }) if t == task_id
        ));
        assert_eq!(reg.open_task_count(&w), 0);
        assert!(updates.recv().await.is_none());

        // A late frame for the cancelled task is dropped, not fatal.
        assert!(reg.on_frame(
            &w,
            ParticipantFrame::Status {
                task_id,
                state: TaskState::Completed,
                message: None,
                metadata: None,
                input: None,
            },
        ));
    }

    #[tokio::test]
    async fn an_answer_reaches_the_participant_that_owns_the_task() {
        use super::super::protocol::{InputAnswer, InputQuestion};

        let reg = ParticipantRegistry::new();
        let (w, mut outbound) = register(&reg, "worker");
        let (task_id, mut updates) = reg.submit_task(&w, DelegatedTask::new("a")).unwrap();
        let _ = outbound.recv().await;

        // The worker parks the task and says what it is waiting for.
        reg.on_frame(
            &w,
            ParticipantFrame::Status {
                task_id: task_id.clone(),
                state: TaskState::InputRequired,
                message: Some("Which database?".into()),
                metadata: None,
                input: Some(InputRequest {
                    id: "req-1".into(),
                    questions: vec![InputQuestion {
                        id: "q1".into(),
                        question: "Which database?".into(),
                        options: vec!["Postgres".into(), "SQLite".into()],
                    }],
                }),
            },
        );
        let Some(TaskUpdate::Status {
            state: TaskState::InputRequired,
            input: Some(request),
            ..
        }) = updates.recv().await
        else {
            panic!("the caller sees the request behind the parked state");
        };
        assert_eq!(request.id, "req-1");
        assert!(reg.owns_task(&task_id), "a parked task is still open");

        // The answer is addressed by task id alone.
        let input = TaskInput {
            request_id: request.id,
            answers: vec![InputAnswer {
                id: "q1".into(),
                answer: "Postgres".into(),
                custom: false,
            }],
        };
        reg.answer_task(&task_id, input.clone()).unwrap();
        assert!(matches!(
            outbound.recv().await,
            Some(BrokerFrame::Input { task_id: t, input: i }) if t == task_id && i == input
        ));
        assert_eq!(
            reg.open_task_count(&w),
            1,
            "answering does not close the task"
        );

        assert_eq!(
            reg.answer_task("task-nobody", input).unwrap_err(),
            AnswerError::UnknownTask("task-nobody".into())
        );
        assert!(!reg.owns_task("task-nobody"));
    }

    #[test]
    fn a_second_hello_frame_is_a_protocol_error() {
        let reg = ParticipantRegistry::new();
        let (w, _outbound) = register(&reg, "worker");
        assert!(!reg.on_frame(
            &w,
            ParticipantFrame::Hello {
                card: card("other")
            }
        ));
    }

    #[test]
    fn agents_are_listed_in_name_order() {
        let reg = ParticipantRegistry::new();
        let _b = register(&reg, "b-worker");
        let _a = register(&reg, "a-worker");
        let names: Vec<String> = reg
            .agents()
            .into_iter()
            .map(|agent| agent.card.name)
            .collect();
        assert_eq!(
            names,
            vec!["a-worker-0".to_string(), "b-worker-0".to_string()]
        );
    }

    /// The origin is the transport's answer, and it survives to the listing —
    /// a hosted worker and a local child are both addressable, and a caller
    /// has to be able to tell them apart (ADR-0011 C5).
    #[test]
    fn each_registration_keeps_the_origin_its_transport_gave_it() {
        let reg = ParticipantRegistry::new();
        let (child, _local) = register_from(&reg, "child", AgentOrigin::Local);
        let (vm, _hosted) = register_from(&reg, "leased-vm", AgentOrigin::Fleet);

        assert_eq!(reg.origin(&child), Some(AgentOrigin::Local));
        assert_eq!(reg.origin(&vm), Some(AgentOrigin::Fleet));
        assert_eq!(reg.origin("nobody"), None);

        let listed: Vec<(String, AgentOrigin)> = reg
            .agents()
            .into_iter()
            .map(|agent| (agent.card.name, agent.origin))
            .collect();
        assert_eq!(
            listed,
            vec![
                ("child-0".to_string(), AgentOrigin::Local),
                ("leased-vm-0".to_string(), AgentOrigin::Fleet),
            ]
        );
    }
}
