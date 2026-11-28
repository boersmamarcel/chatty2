//! Calls to other agents, run by the broker as the node that made them
//! (ADR-0020, BI-4, AGE-636).
//!
//! A worker's `invoke_agent` and `list_agents` arrive as `call` frames on
//! the connection the broker made for it; the root's arrive through a
//! [`DirectTransport`], an in-process handle with no socket and no HTTP hop.
//! Either way the broker already knows who is calling — the connection names
//! the node, and the direct handle is the root's — so a call carries no
//! claim about its caller.
//!
//! An `invoke_agent` call runs exactly as an A2A request to the same agent
//! would: a registered participant gets the task, a virtual agent starts a
//! worker for it, and the task's updates become the call's progress and
//! result. Dropping the call's stream — the caller cancelled, or its
//! connection closed — cancels the task and reaps the worker, the same way a
//! caller hanging up on the SSE stream does.
//!
//! The runs of one root form a tree (ADR-0092): every task a node is handed
//! records the run its call was made from. A stop or a deadline on a task
//! stops its whole subtree first, bottom-up — the subtree is marked so
//! nothing in it starts another run, then its deepest level is stopped and
//! waited for until its workers are gone, then the level above — and only
//! then does the task's own call report its end, within the call's
//! [`deadline_grace`]. A caller that hangs up cannot wait, so dropping a
//! call sends the same stops in the same order without waiting; and each
//! hop's worker still dies with the call that started it, its own calls
//! with its connection.
//!
//! A `send_message` call (tree messages, TM-1) never starts a run: the
//! gate checks the recipient against the caller — a node's owner, a node's
//! own child, or (from the human, through the root) any node — and the
//! broker queues the message on the recipient's [`PendingList`], or
//! refuses it. Either way the call's result is a [`MessageStatus`].
//!
//! A message to an owner is delivered at exactly two points (TM-2): on the
//! next `invoke_agent` result its recipient receives, as that result's
//! `messages`, or at the start of the recipient's next run — the root's
//! next user turn ([`Transport::take_run_messages`]), a node's next task
//! ([`BrokerCalls::start_run`]). Never mid-run.
//!
//! A message from the human or from a node's owner goes on the node's
//! mid-run list instead (TM-5), which the node takes between two tool
//! rounds with `mailbox.take` — one `delivered_mid_run` edge-log row per
//! message — or, if no tool round took it, at its next run's start.
//!
//! Every message is delivered wrapped as untrusted data
//! ([`chatty_fabric::wrap_message`]) and grants nothing. A recipient that
//! ends drops what is still waiting for it, one edge-log row per message
//! ([`BrokerCalls::recipient_ended`]).
//!
//! A worker the call starts is spawned with a [`SpawnContext`] the broker
//! sets from the caller's own (BI-5): a sub-leader's child gets its tree
//! under the sub-leader's and its branch off the sub-leader's, and may call
//! only what the sub-leader may. A context the call brings is clamped to the
//! caller's; see [`super::spawn_context`].
//!
//! A question climbs the caller chain (ADR-0021 § 2, EN-2b). A node's
//! `human.ask` reaches [`BrokerCalls::raise_question`], which stamps the
//! asker from the node's admitted name and the chain of the call running it
//! (overwriting whatever the worker said), gives it an id unique within
//! this broker, and relays it to whoever made that call: a calling node as
//! a broker→worker `human.ask` on its connection, the root call it is
//! nested under as [`CallEvent::Ask`]. A node that answers `escalate` sends
//! the same request, first stamp intact, to its own caller, and so on up;
//! the answers go back to the request that asked, and nowhere else. A
//! worker that withdraws its question, or whose connection closes because
//! its callee ended or was stopped, withdraws it from wherever it waits: a
//! `req.cancel` to the node it was relayed to, or the root's popover
//! ([`CallEvent::InputWithdrawn`] under the broker's id). A call that ends
//! withdraws whatever its callee still asks. Like an approval, a question
//! whose root call or asker's call ends gets no result at all: the asker
//! waits until it is reaped with that call's subtree, and never acts on a
//! reply in between. The gate decides first: a node asks only from the one
//! open run it serves, and only the root answers.
//!
//! An approval does not climb: only the root answers one (ADR-0021 § 2,
//! EN-2a). A node's `human.approve` reaches [`BrokerCalls::raise_approval`],
//! which stamps the asker from the node's admitted name and the chain of
//! the run it serves (overwriting whatever the worker said), gives it an id
//! unique within this broker, and delivers it to the root call that run is
//! nested under as [`CallEvent::Approve`] — beside the swarm events, on the
//! same line. The root's answer ([`Transport::approve`]) goes back to the
//! request that asked, and nowhere else. A worker that withdraws its
//! request, or whose connection closes because its callee ended or was
//! stopped, withdraws the root's card ([`CallEvent::InputWithdrawn`] under
//! the broker's id). A root call that ends answers nothing still pending
//! under it: the asker is in the subtree that goes with the call, and a
//! verdict sent now would race its reaping and let it act on a call that
//! is over. An approval with no root call to go to is denied.
//!
//! A caller metered on a model endpoint does not hold its permit while it
//! waits (BI-6): an `invoke_agent` call releases the caller's
//! [`RunPermit`](chatty_fabric::RunPermit), and the call that brings the
//! caller's outstanding count back to zero re-acquires it, in the endpoint's
//! queue, before its result is delivered — the result is what starts the
//! caller's next model call. So a sub-leader and its child can share a
//! budget-1 endpoint. A call dropped while it waits takes nothing and
//! delivers nothing.
//!
//! Every request — a call over a worker's connection, or the root's over
//! its direct handle — passes the broker gate before any effect
//! ([`super::gate`], ADR-0023, GT-0): the broker resolves the typed
//! [`Caller`] from the [`Peer`] that authenticated the request and the task
//! table, copies what the decision needs into a [`Snapshot`], and acts only
//! on the [`Grant`] [`decide`] returns, on the target it names. Every
//! decision is logged with the typed caller and the row it matched before
//! anything happens. A node calls from a run it serves, and its
//! `agent.invoke` names that run (GT-0b); a node with no open run is a
//! chainless `External`, refused everywhere.
//!
//! Before an `invoke_agent` call spawns, submits or permits anything, the
//! gate checks it (PL-S2): a node's call against the specs'
//! [`CallPolicy`] (`delegates_to`, `exposed`, `callers`; DP-1's
//! `may_call`), then every call against its [`CallChain`] (DP-2). The chain
//! is the named run's own, from the broker's task table, plus the callee:
//! a call frame says nothing about where its caller is, and anything extra
//! it carries is dropped when it is parsed. A call that would close a cycle
//! or go deeper than [`MAX_DEPTH`](chatty_fabric::MAX_DEPTH) ends with
//! [`CallError::Delegation`] and a refusal row; the model reads the typed
//! reason. The root's calls are checked against the spec the root runs as,
//! when it runs as one — a `--team` leader, an `--agent <spec>` root — and
//! refused the same way (AGE-745); a plain root's own tools decide whether
//! it may delegate at all, so the policy lets it call anyone.
//!
//! Then the budget (DP-3): what the caller has left is its chain's budget
//! narrowed by what the call says the caller has left of it (the caller's
//! own count of its turns and dollars spent, delegated usage included), and
//! a call with a limit used up ends with `budget_spent: turns|seconds|usd`
//! before anything is spawned. The callee runs under the tighter of that and
//! its own spec's budget, which its task frame carries. A callee still going
//! past its deadline (plus [`deadline_grace`]) is stopped: the call ends
//! with a failed result, never a hang.
//!
//! A root call hears about every run nested under it (TB-1): a run a
//! node's call starts, whose root call is listening, is asked for its
//! turns and tool events, and the broker forwards them — with the run's
//! text summarised to its length, its usage and its end — to the root
//! call's stream as [`CallEvent::Swarm`], tagged with the node and the
//! chain from the task table. The root call flushes them at most once per
//! [`FORWARD_INTERVAL`], one batch per node, and the last of them before
//! its result.
//!
//! The root can stop one run while the rest of the swarm keeps going (TB-7,
//! AGE-749): [`BrokerCalls::cancel`] names a node — or, for one of the
//! root's own callees the tree still knows only by its spec, that spec —
//! and the call that started it ends with a [`CANCELLED_BY_USER`] result.
//! Its worker is reaped as the call lets go of it, which closes its
//! connection and so drops every call it made: the subtree goes the way a
//! hung-up caller's does, permits and pending messages with it. The
//! caller's own run carries on, and a question or an approval the stopped
//! subtree was waiting on is withdrawn from wherever it waits.
//!
//! Every `invoke_agent` call writes one row to the broker's edge log when it
//! ends, every `send_message` call one message row, and every refused call
//! one refusal row. A stopped call's task row says `cancelled`. A task row
//! whose callee reported usage carries its price, or `unpriced`, when the
//! broker has a [`UsagePricer`]. `list_agents` reads the directory and is
//! not an edge between two nodes, so it writes none.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use chatty_fabric::wire::{AgentEntry, Opaque, TaskMetadata, WireProgress};
use chatty_fabric::{
    AgentOrigin, Answer, ApprovalRequest, ApprovalVerdict, AskReply, AskRequest, Asker,
    CANCELLED_BY_USER, CallChain, CallError, CallEvent, CallPolicy, CallRequest, CallResult,
    CallStream, CapturedConversation, ChildCall, Clock, ConversationScope, EdgeKind, EdgeLog,
    EdgeRow, FORWARD_INTERVAL, Handle, HandleKey, HandleOwner, HandleParty, HandleSnapshot,
    HandleStatus, HandleStore, HandleTicket, InvokeAgentOutcome, InvokeAgentParams, Message,
    MessageStatus, NodeId, NodeState, PendingList, ROOT_NAME, Refusal, RefusalReason, RunRoot,
    SavedConversation, SendMessageParams, Sender, SwarmBatcher, SwarmItem, Transport, UsagePricer,
    deadline_grace,
};
use futures::StreamExt;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, error, info, warn};

use super::gate::{
    self, Addressee, Admitter, BrokerRoot, Callee, Caller, Decision, Delivery, Grant, HandleCheck,
    InvokeTarget, NodeCaller, Owner, PostTo, Refused, Request, Snapshot, SpawnView, Unreadable,
};
use super::hosted::Hosted;
use super::protocol::{CallStamp, DelegatedTask, TaskState};
use super::recent_tasks::{RecentTasks, TaskRecord};
use super::registry::{ParticipantRegistry, ROOT_SCOPE, TaskUpdate};
use super::spawn_context::{self, Target};
use super::virtual_agent::VirtualAgent;
use crate::handlers::a2a_participant;

/// Who is on the other end of a request, as its transport authenticated
/// it: the root's direct handle, or the connection the broker made for a
/// node. The gate resolves the typed [`Caller`] from it and the task table,
/// per request (ADR-0023 § 2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Peer {
    /// The in-process root, through its [`DirectTransport`].
    Root,
    /// A node, by the name its connection was admitted under.
    Node(String),
}

impl Peer {
    fn name(&self) -> &str {
        match self {
            Self::Root => ROOT_NAME,
            Self::Node(name) => name,
        }
    }
}

/// Whose pending list a message waits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Recipient {
    /// The in-process root, which is not a node.
    Root,
    Node(NodeId),
    /// A node's mid-run list (TM-5): what the human or the node's owner
    /// sent it, which its next tool round takes. TM-2's delivery point a
    /// never reads it.
    MidRun(NodeId),
}

/// The broker's call path: the agents a call can reach, the messages
/// waiting for them, and the log it writes. Shared by every connection and
/// the root's direct handle.
pub struct BrokerCalls {
    registry: ParticipantRegistry,
    runners: Arc<BTreeMap<String, Arc<dyn VirtualAgent>>>,
    edges: Option<Arc<Mutex<EdgeLog>>>,
    /// Messages waiting for each recipient (tree messages), until the next
    /// `invoke_agent` result the recipient receives or its next run takes
    /// them (TM-2). Shared with each call's stream, which delivers on its
    /// result.
    pending: Arc<Mutex<HashMap<Recipient, PendingList>>>,
    /// Messages waiting for an idle handle, by its key (TM-4): the handle's next task opens with them. Its workers
    /// are fresh nodes, so no node's list could hold them.
    handle_pending: Arc<Mutex<HashMap<HandleKey, PendingList>>>,
    next_message: AtomicU64,
    /// The spec rules a call is checked against (PL-S2, AGE-745). Always
    /// one: a broker nobody gave a policy consults [`LocalPermissive`].
    policy: Policy,
    /// Prices a callee's reported usage for its task row (DP-3); `None`
    /// writes no price.
    pricer: Option<Arc<dyn UsagePricer>>,
    /// Each root call in flight, by its root task id: where the runs
    /// nested under it report (TB-1).
    swarm: Swarm,
    /// Every running call, by the node it runs: what
    /// [`cancel`](BrokerCalls::cancel) stops (TB-7).
    stops: Stops,
    /// Every approval waiting on the root, by the id this broker gave it
    /// (EN-2a).
    approvals: Approvals,
    /// The last approval id handed out.
    next_approval: AtomicU64,
    /// Every question waiting on a caller, by the id this broker gave it
    /// (EN-2b).
    questions: Questions,
    /// The last question id handed out.
    next_question: AtomicU64,
    /// Who called each node a call is running, by the node's name: where
    /// its questions go (EN-2b).
    routes: Routes,
    /// The binding, decision log and answer nonces of a hosted broker
    /// (HS-4a); `None` on a local one.
    hosted: Option<Hosted>,
    /// The resumable handles callers named (RC-3): their saved
    /// conversations and trees, and the queue each one's calls wait in.
    /// In this broker's memory only.
    handles: Arc<HandleStore>,
    /// How often a call waiting in a handle's queue tells its caller it is
    /// still alive (RC-3): the caller's stall watchdog hears nothing else
    /// while the call ahead of it runs.
    queued_heartbeat: std::time::Duration,
    /// The root conversation of each root call in flight, by its root task
    /// id: the conversation every run nested under it works for (RC-4).
    scopes: Scopes,
    /// The tasks this broker ran, live or finished within
    /// [`RECENT_TASKS`](super::recent_tasks::RECENT_TASKS): what `tasks/get`
    /// answers (RC-4).
    tasks: Arc<Mutex<RecentTasks>>,
    /// What the broker tells the time by (handles' idleness, recent tasks).
    clock: Clock,
    /// Runs after the idle check and before the push of a message to a
    /// handle, inside the pending lock (tests interleave a clear there).
    #[cfg(test)]
    before_handle_push: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
}

/// Root conversations by root task id.
type Scopes = Arc<Mutex<HashMap<String, ConversationScope>>>;

/// A root call's entry in [`BrokerCalls::scopes`], removed when the call
/// ends.
struct ScopeGuard {
    scopes: Scopes,
    root_task_id: String,
}

impl Drop for ScopeGuard {
    fn drop(&mut self) {
        locked(&self.scopes).remove(&self.root_task_id);
    }
}

/// A plain call's spawned worker: when its call ends, the handles it owns
/// end (RC-4).
struct OwnerGuard {
    handles: Arc<HandleStore>,
    handle_pending: Arc<Mutex<HashMap<HandleKey, PendingList>>>,
    edges: Option<Arc<Mutex<EdgeLog>>>,
    scope: ConversationScope,
    node: String,
}

impl Drop for OwnerGuard {
    fn drop(&mut self) {
        self.handles
            .owner_ended(&self.scope, &HandleParty::Node(self.node.clone()));
        drop_ended_handle_mail(&self.handles, &self.handle_pending, &self.edges);
    }
}

/// Lock `mutex`, past a poisoning panic: everything behind these locks is
/// plain owned data with no invariant a panic could leave half-kept.
fn locked<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// Marks a task cancelled in the recent tasks when its call is dropped
/// before the task ended; a task that ended keeps its state.
struct TaskEnd {
    tasks: Arc<Mutex<RecentTasks>>,
    id: String,
    clock: Clock,
}

impl Drop for TaskEnd {
    fn drop(&mut self) {
        locked(&self.tasks).end(&self.id, TaskState::Canceled, (self.clock)());
    }
}

/// What an `agent.invoke` carries about handles and conversations into its
/// stream (RC-3, RC-4).
struct CallHandle {
    /// The caller's root conversation.
    scope: ConversationScope,
    /// The call's place on the handle it names; `None` for a plain call, or
    /// one to a registered participant (refused in the stream).
    ticket: Option<HandleTicket>,
    /// A root call's entry in the scopes table.
    root: Option<ScopeGuard>,
}

/// [`BrokerCalls`]' default interval between the heartbeats of a call
/// waiting in a handle's queue: chatty-core's `HEARTBEAT_INTERVAL`, far
/// below the caller's tool stall timeout. The embedder passes the scaled
/// one with [`BrokerCalls::with_queued_heartbeat`].
pub const QUEUED_HEARTBEAT: std::time::Duration = std::time::Duration::from_secs(30);

/// Approvals waiting on the root, by broker id.
type Approvals = Arc<Mutex<HashMap<String, PendingApproval>>>;

/// One approval the root has been asked for and has not answered.
struct PendingApproval {
    /// The root call it was delivered to: when that call ends, it is denied.
    root_task_id: String,
    /// Where the root's answer goes: the request that asked.
    answer: oneshot::Sender<ApprovalVerdict>,
    /// The root call's line, for a withdrawal.
    root: mpsc::UnboundedSender<ToRoot>,
}

/// A node's approval, as [`BrokerCalls::raise_approval`] raised it: wait on
/// [`verdict`](Self::verdict). Dropping it before the root answered — the
/// worker withdrew the request, or its connection closed — withdraws the
/// root's card.
pub(crate) struct RaisedApproval {
    /// `None` for an approval denied without asking.
    pending: Option<(String, oneshot::Receiver<ApprovalVerdict>)>,
    approvals: Approvals,
}

impl RaisedApproval {
    fn denied(approvals: &Approvals) -> Self {
        Self {
            pending: None,
            approvals: approvals.clone(),
        }
    }

    /// The root's answer; `Denied` when there was nobody to ask. Never,
    /// once the root call it went to has ended: the asker is reaped with
    /// that call's subtree, and must not act before it is.
    pub(crate) async fn verdict(&mut self) -> ApprovalVerdict {
        match self.pending.as_mut() {
            Some((_, answer)) => match answer.await {
                Ok(verdict) => verdict,
                Err(_) => std::future::pending().await,
            },
            None => ApprovalVerdict::Denied,
        }
    }
}

impl Drop for RaisedApproval {
    fn drop(&mut self) {
        let Some((id, _)) = self.pending.take() else {
            return;
        };
        // Answered approvals are already out of the table.
        let pending = locked(&self.approvals).remove(&id);
        if let Some(pending) = pending {
            debug!(approval = %id, "An approval was withdrawn before the root answered");
            let _ = pending.root.send(ToRoot::Withdrawn { id });
        }
    }
}

/// The caller of each node a call is running, by the node's name.
type Routes = Arc<Mutex<HashMap<String, Route>>>;

/// One running call, as its callee's questions see it.
#[derive(Clone)]
struct Route {
    /// Which call: the entry is its own to take off again.
    call: u64,
    caller: Peer,
    /// The chain the broker stamped on the call: its root call's id, and
    /// the specs from the root to the callee.
    chain: CallChain,
}

/// A running call's entry in [`Routes`]. When the call ends — its callee
/// finished, failed or was stopped — it leaves, and every question its
/// callee asked is withdrawn from wherever it waits (EN-2b).
struct RouteGuard {
    routes: Routes,
    questions: Questions,
    registry: ParticipantRegistry,
    node: String,
    call: u64,
}

impl Drop for RouteGuard {
    fn drop(&mut self) {
        {
            let mut routes = locked(&self.routes);
            if !routes
                .get(&self.node)
                .is_some_and(|route| route.call == self.call)
            {
                return;
            }
            routes.remove(&self.node);
        }
        let asked: Vec<(String, PendingQuestion)> = {
            let mut questions = locked(&self.questions);
            let ids: Vec<String> = questions
                .iter()
                .filter(|(_, pending)| pending.asker == self.node)
                .map(|(id, _)| id.clone())
                .collect();
            ids.into_iter()
                .filter_map(|id| questions.remove(&id).map(|pending| (id, pending)))
                .collect()
        };
        // Dropping each reply leaves the asker's wait unanswered.
        for (id, pending) in asked {
            debug!(question = %id, node = %self.node, "The asker's call ended; withdrawing its question");
            withdraw(&self.registry, &id, pending.at);
        }
    }
}

/// Take question `id` off wherever it waits: the root's popover, or the
/// node it was relayed to.
fn withdraw(registry: &ParticipantRegistry, id: &str, at: Hop) {
    match at {
        Hop::Root(root) => {
            let _ = root.send(ToRoot::Withdrawn { id: id.to_string() });
        }
        Hop::Node(caller) => registry.withdraw_question(&caller, id),
    }
}

/// Questions waiting on a caller, by broker id.
type Questions = Arc<Mutex<HashMap<String, PendingQuestion>>>;

/// One question a caller has been asked and has not answered.
struct PendingQuestion {
    /// Where it waits now.
    at: Hop,
    /// The node that asked it.
    asker: String,
    /// The root call the hop it waits at is nested under: when that call
    /// ends, the question goes unanswered.
    root_task_id: String,
    /// Where this hop's reply goes: the question's wait.
    reply: oneshot::Sender<AskReply>,
}

/// Where a question waits.
#[derive(Clone)]
enum Hop {
    /// On a root call's stream, as [`CallEvent::Ask`].
    Root(mpsc::UnboundedSender<ToRoot>),
    /// On a calling node's connection, as a broker→worker `human.ask`.
    Node(String),
}

/// A node's question, as [`BrokerCalls::raise_question`] raised it: wait on
/// [`answers`](Self::answers), which climbs the caller chain. Dropping it
/// before it was answered — the worker withdrew the request, or its
/// connection closed — withdraws it from wherever it waits.
pub(crate) struct RaisedQuestion {
    id: String,
    /// The node that asked.
    asker: String,
    /// The node whose caller is asked next: the asker, then each node that
    /// escalated.
    from: String,
    request: AskRequest,
    registry: ParticipantRegistry,
    routes: Routes,
    swarm: Swarm,
    questions: Questions,
}

impl RaisedQuestion {
    /// The answers, from the first caller up the chain that gave any.
    /// `Err` when nobody is left to ask. Never, once the call it waited on
    /// or the asker's own call has ended: the asker is reaped with that
    /// call's subtree, and must not act before it is.
    pub(crate) async fn answers(&mut self) -> Result<Vec<Answer>, CallError> {
        let unanswered = || CallError::Failed("the question went unanswered".to_string());
        loop {
            let (reply, replied) = oneshot::channel();
            let Some(route) = locked(&self.routes).get(&self.from).cloned() else {
                warn!(participant = %self.from, question = %self.id, "A question with nobody to ask");
                return Err(nobody_above());
            };
            // The root call this hop's call is nested under.
            let root_task_id = route.chain.root_task_id;
            let hop = match route.caller {
                Peer::Root => {
                    let Some(root) = locked(&self.swarm).get(&root_task_id).cloned() else {
                        warn!(question = %self.id, "A question with no root call to ask");
                        return Err(unanswered());
                    };
                    Hop::Root(root)
                }
                Peer::Node(caller) => Hop::Node(caller),
            };
            // In the table before it is sent, so an answer — or a root call
            // that ends — in between finds it.
            locked(&self.questions).insert(
                self.id.clone(),
                PendingQuestion {
                    at: hop.clone(),
                    asker: self.asker.clone(),
                    root_task_id,
                    reply,
                },
            );
            let sent = match &hop {
                Hop::Root(root) => {
                    info!(question = %self.id, "A question reached the root");
                    root.send(ToRoot::Ask {
                        id: self.id.clone(),
                        request: self.request.clone(),
                    })
                    .is_ok()
                }
                Hop::Node(caller) => {
                    debug!(question = %self.id, caller = %caller, "Relaying a question to its asker's caller");
                    self.registry
                        .relay_question(caller, &self.id, self.request.clone())
                        .await
                }
            };
            if !sent {
                locked(&self.questions).remove(&self.id);
                return Err(unanswered());
            }
            match replied.await {
                Ok(AskReply::Answers(answers)) => return Ok(answers),
                Ok(AskReply::Escalate) => match hop {
                    Hop::Node(caller) => {
                        debug!(question = %self.id, caller = %caller, "A caller escalated a question");
                        self.from = caller;
                    }
                    // The root has nobody to escalate to.
                    Hop::Root(_) => return Err(unanswered()),
                },
                // The call the question waited on ended, or the asker's
                // own did: like an approval (EN-2a), it is never answered
                // then, and the asker is reaped with the call's subtree
                // before it can act on a reply.
                Err(_) => std::future::pending::<()>().await,
            }
        }
    }
}

impl Drop for RaisedQuestion {
    fn drop(&mut self) {
        // Answered questions are already out of the table.
        let Some(pending) = locked(&self.questions).remove(&self.id) else {
            return;
        };
        debug!(question = %self.id, "A question was withdrawn before it was answered");
        withdraw(&self.registry, &self.id, pending.at);
    }
}

/// The running calls [`BrokerCalls::cancel`] can stop.
type Stops = Arc<Mutex<Vec<Stoppable>>>;

/// One running call: the node its callee runs as, the task it runs, the
/// name its caller addressed, and the line that stops it.
struct Stoppable {
    id: u64,
    node: String,
    task_id: String,
    agent: String,
    by_root: bool,
    stop: oneshot::Sender<Stop>,
}

/// A stop sent to a running call. `reaped`, when set, hears once the
/// call's worker and everything under it are gone (ADR-0092): a cancel
/// above it waits for that before it stops the next level up.
#[derive(Default)]
struct Stop {
    reaped: Option<oneshot::Sender<()>>,
}

/// Take the entries running any of `task_ids` off `stops`.
fn take_stops(stops: &Stops, task_ids: &[String]) -> Vec<Stoppable> {
    let mut stops = locked(stops);
    let (taken, running): (Vec<_>, Vec<_>) = stops
        .drain(..)
        .partition(|entry| task_ids.contains(&entry.task_id));
    *stops = running;
    taken
}

/// Stop every run under the task `task_id` that `node` serves, bottom-up
/// (ADR-0092): mark the subtree so none of it starts anything more, then
/// stop its deepest level, wait until each of those calls has reaped its
/// worker, and go on to the level above, all by `until`. Called before the
/// task's own call reports its end, so a cancelled or timed-out task ends
/// with nothing left running under it. A level that is not gone by
/// `until` is left to go with its caller's connection.
async fn stop_subtree(
    stops: &Stops,
    registry: &ParticipantRegistry,
    node: &str,
    task_id: &str,
    until: tokio::time::Instant,
) {
    let levels = registry.begin_cancel(node, task_id);
    for level in levels {
        let reaped: Vec<_> = take_stops(stops, &level)
            .into_iter()
            .map(|entry| {
                let (tx, rx) = oneshot::channel();
                debug!(node = %entry.node, under = %node, "Stopping a run under a cancelled one");
                let _ = entry.stop.send(Stop { reaped: Some(tx) });
                rx
            })
            .collect();
        if tokio::time::timeout_at(until, futures::future::join_all(reaped))
            .await
            .is_err()
        {
            warn!(node = %node, "The runs under a cancelled task were not all gone in time");
            return;
        }
    }
}

/// A running call's entry in [`Stops`]; leaves it when the call ends.
struct StopGuard {
    stops: Stops,
    id: u64,
}

impl Drop for StopGuard {
    fn drop(&mut self) {
        locked(&self.stops).retain(|entry| entry.id != self.id);
    }
}

/// Numbers [`Stoppable`] entries, unique in this process.
static NEXT_STOPPABLE: AtomicU64 = AtomicU64::new(0);

/// Put the call running `node` for `caller`, addressed as `agent`, on
/// `stops`: the receiver fires when the user stops it, and the guard takes
/// it off again.
fn stoppable(
    stops: &Stops,
    caller: &Peer,
    agent: &str,
    node: &str,
    task_id: &str,
) -> (oneshot::Receiver<Stop>, StopGuard) {
    let id = NEXT_STOPPABLE.fetch_add(1, Ordering::Relaxed);
    let (stop, stopped) = oneshot::channel();
    locked(stops).push(Stoppable {
        id,
        node: node.to_string(),
        task_id: task_id.to_string(),
        agent: agent.to_string(),
        by_root: *caller == Peer::Root,
        stop,
    });
    (
        stopped,
        StopGuard {
            stops: stops.clone(),
            id,
        },
    )
}

/// Root calls listening for their nested runs, by root task id.
type Swarm = Arc<Mutex<HashMap<String, mpsc::UnboundedSender<ToRoot>>>>;

/// What reaches a root call from the runs nested under it.
enum ToRoot {
    /// A nested run's event (TB-1).
    Nested(Nested),
    /// A node's approval, for the root to answer (EN-2a).
    Approve {
        id: String,
        request: ApprovalRequest,
    },
    /// A node's question every caller below the root escalated, for the
    /// root to answer (EN-2b).
    Ask { id: String, request: AskRequest },
    /// That approval or question is over without the root's answer.
    Withdrawn { id: String },
}

/// One item a nested run reported, with the broker's tag.
struct Nested {
    node: String,
    chain: CallChain,
    item: SwarmItem,
}

/// A root call's end of [`Swarm`]; stops listening when dropped, which
/// drops every approval and every question still waiting on it
/// unanswered.
struct Listening {
    swarm: Swarm,
    approvals: Approvals,
    questions: Questions,
    root_task_id: String,
    nested: mpsc::UnboundedReceiver<ToRoot>,
}

impl Listening {
    fn open(
        swarm: &Swarm,
        approvals: &Approvals,
        questions: &Questions,
        root_task_id: &str,
    ) -> Self {
        let (tx, nested) = mpsc::unbounded_channel();
        locked(swarm).insert(root_task_id.to_string(), tx);
        Self {
            swarm: swarm.clone(),
            approvals: approvals.clone(),
            questions: questions.clone(),
            root_task_id: root_task_id.to_string(),
            nested,
        }
    }
}

impl Drop for Listening {
    fn drop(&mut self) {
        locked(&self.swarm).remove(&self.root_task_id);
        let mut approvals = locked(&self.approvals);
        let orphaned: Vec<String> = approvals
            .iter()
            .filter(|(_, pending)| pending.root_task_id == self.root_task_id)
            .map(|(id, _)| id.clone())
            .collect();
        // Unanswered: dropping the answer's sender leaves the asker
        // waiting until its connection closes with the call's subtree.
        for id in orphaned {
            approvals.remove(&id);
        }
        drop(approvals);
        // Dropping a question's reply leaves it unanswered.
        locked(&self.questions).retain(|_, pending| {
            !(matches!(pending.at, Hop::Root(_)) && pending.root_task_id == self.root_task_id)
        });
    }
}

/// A nested run's line to its root call: where it reports, and the tag the
/// broker puts on what it reports.
struct Reporting {
    to: mpsc::UnboundedSender<ToRoot>,
    chain: CallChain,
    node: String,
    /// Whether the run's end has been reported.
    ended: AtomicBool,
}

/// A run whose call went away before its end — its caller was stopped or
/// hung up (TB-7) — ends as canceled, so the root's tree does not show it
/// running on.
impl Drop for Reporting {
    fn drop(&mut self) {
        if !self.ended.load(Ordering::Relaxed) {
            self.send(SwarmItem::Ended {
                state: TaskState::Canceled.to_string(),
            });
        }
    }
}

impl Reporting {
    fn send(&self, item: SwarmItem) {
        if matches!(item, SwarmItem::Ended { .. }) {
            self.ended.store(true, Ordering::Relaxed);
        }
        let _ = self.to.send(ToRoot::Nested(Nested {
            node: self.node.clone(),
            chain: self.chain.clone(),
            item,
        }));
    }
}

/// When a call whose callee outran its deadline is stopped (DP-3), or
/// never for a call without one.
async fn sleep_until_cut(cut: Option<tokio::time::Instant>) {
    match cut {
        Some(cut) => tokio::time::sleep_until(cut).await,
        None => std::future::pending().await,
    }
}

/// The next item a root call hears, or never for a call that does not
/// listen.
async fn next_nested(listening: &mut Option<Listening>) -> Option<ToRoot> {
    match listening {
        Some(listening) => listening.nested.recv().await,
        None => std::future::pending().await,
    }
}

/// Why a question has nowhere to go: no call is running its asker, or none
/// is running the node it was escalated to.
fn nobody_above() -> CallError {
    CallError::Failed("nobody above this agent can answer its question".to_string())
}

/// The delegation policy of a local broker nobody gave one: every caller,
/// the root included, may call every spec, and no spec brings a budget of
/// its own, so only the call chain (cycle, depth, the caller's budget)
/// limits a call. Right for a plain local root, whose own tools decide
/// whether it may delegate at all (AGE-745).
///
/// A named value, not a skipped check, so that nothing inherits it
/// silently. Its constructor is crate-private on purpose: only the local
/// and desktop builders here ([`BrokerCalls::new`],
/// [`BrokerCalls::with_policy`] given `None`) make one. The hosted broker
/// (HS-4a, AGE-685) takes a [`CallPolicy`] by value and must not be able to
/// build this one: without a real policy, a hosted broker does not start
/// (ADR-0021, Migration step 4).
#[derive(Debug)]
pub struct LocalPermissive(());

impl LocalPermissive {
    pub(crate) fn new() -> Self {
        Self(())
    }
}

impl CallPolicy for LocalPermissive {
    fn may_call(&self, _caller: &str, _callee: &str) -> Result<(), Refusal> {
        Ok(())
    }

    fn root_may_call(&self, _callee: &str) -> Result<(), Refusal> {
        Ok(())
    }
}

/// The policy a broker checks calls against: the local default by name, or
/// the one it was given.
enum Policy {
    LocalPermissive(LocalPermissive),
    Configured(Arc<dyn CallPolicy>),
}

impl Policy {
    fn get(&self) -> &dyn CallPolicy {
        match self {
            Policy::LocalPermissive(policy) => policy,
            Policy::Configured(policy) => policy.as_ref(),
        }
    }
}

impl BrokerCalls {
    pub fn new(
        registry: ParticipantRegistry,
        runners: Arc<BTreeMap<String, Arc<dyn VirtualAgent>>>,
        edges: Option<Arc<Mutex<EdgeLog>>>,
    ) -> Self {
        Self {
            registry,
            runners,
            edges,
            pending: Arc::default(),
            handle_pending: Arc::default(),
            #[cfg(test)]
            before_handle_push: Mutex::default(),
            next_message: AtomicU64::new(0),
            policy: Policy::LocalPermissive(LocalPermissive::new()),
            pricer: None,
            swarm: Arc::default(),
            stops: Arc::default(),
            approvals: Arc::default(),
            next_approval: AtomicU64::new(0),
            questions: Arc::default(),
            next_question: AtomicU64::new(0),
            routes: Arc::default(),
            hosted: None,
            handles: Arc::default(),
            queued_heartbeat: QUEUED_HEARTBEAT,
            scopes: Arc::default(),
            tasks: Arc::default(),
            clock: Arc::new(std::time::SystemTime::now),
        }
    }

    /// Tell the time by `clock`: a handle's idleness and how long a
    /// finished task is answered for (tests move a fake one).
    #[cfg(test)]
    pub(crate) fn with_clock(mut self, clock: Clock) -> Self {
        self.handles = Arc::new(HandleStore::with_clock(clock.clone()));
        self.clock = clock;
        self
    }

    /// The broker's handles (tests read them).
    #[cfg(test)]
    pub(crate) fn handles(&self) -> &HandleStore {
        &self.handles
    }

    /// The live handles of `scope` with the messages waiting for each, for
    /// the roster (RO-4).
    pub fn handle_rows(&self, scope: &ConversationScope) -> Vec<HandleSnapshot> {
        let listing = self.handles.listing(scope);
        let pending = locked(&self.handle_pending);
        listing
            .into_iter()
            .map(|row| {
                let waiting = pending
                    .get(&HandleKey::new(scope.clone(), row.name.clone()))
                    .map_or(0, PendingList::len);
                HandleSnapshot {
                    owner: match row.owner.owner() {
                        HandleParty::Root => ROOT_NAME.to_string(),
                        HandleParty::Handle(name) | HandleParty::Node(name) => name.clone(),
                    },
                    busy: row.busy,
                    pending_messages: u32::try_from(waiting).unwrap_or(u32::MAX),
                    idle_for_s: row.idle_for.as_secs(),
                    agent: row.agent,
                    tree: row.tree,
                    name: row.name,
                }
            })
            .collect()
    }

    /// The root conversation `scope` ended (RC-4): every handle of it ends,
    /// its saved conversation deleted.
    pub fn end_conversation(&self, scope: &ConversationScope) {
        self.handles.end_conversation(scope);
        self.drop_ended_handle_mail();
    }

    /// Drop what waits for handles that are no longer live, one `dropped`
    /// row per message (TM-4).
    fn drop_ended_handle_mail(&self) {
        drop_ended_handle_mail(&self.handles, &self.handle_pending, &self.edges);
    }

    /// End the handle `name` of `peer`'s conversation for its owner (RC-4):
    /// the gate's handle rule decides, and the handle's saved conversation
    /// is deleted; its tree stays (ADR-0017). `scope` is the root's
    /// conversation, for a root `peer`; `run` is the open run a node drops
    /// it from, which names its conversation, as an `agent.invoke`'s does.
    pub fn drop_handle(
        &self,
        peer: &Peer,
        scope: Option<&ConversationScope>,
        run: Option<&str>,
        name: &str,
    ) -> Result<(), CallError> {
        let caller = self.resolve(peer);
        let request = Request::DropHandle { name, run };
        match self.gate_in(peer, scope, &caller, &request).outcome {
            Ok(_) => {
                let key = HandleKey::new(self.conversation_of(peer, scope, run), name);
                self.handles.drop_handle(&key);
                // The handle's own, and those of the handles it owned.
                drop_handle_mail(&self.handle_pending, &self.edges, &key);
                self.drop_ended_handle_mail();
                Ok(())
            }
            Err(refused) => {
                let error = refused.to_call_error();
                self.log_refusal(peer, "handle.drop", &error.to_string());
                Err(error)
            }
        }
    }

    /// A2A `tasks/get` for the root of `scope` (RC-4): the task `id` of
    /// that conversation, live or finished within
    /// [`RECENT_TASKS`](super::recent_tasks::RECENT_TASKS), as an A2A
    /// `Task`; its `contextId` is the handle it ran under. `None` for a
    /// task this root process never ran for the conversation, or one that
    /// finished longer ago.
    pub fn tasks_get(
        &self,
        scope: Option<&ConversationScope>,
        id: &str,
    ) -> Result<Option<serde_json::Value>, CallError> {
        self.root_request(&Request::TaskGet { id })?;
        let scope = self.conversation_of(&Peer::Root, scope, None);
        Ok(locked(&self.tasks)
            .get(id, &scope, (self.clock)())
            .map(|record| record.to_a2a(id)))
    }

    /// A hosted broker's call path (HS-4a): `policy` is required, so the
    /// local default is never inherited, every decision is written to
    /// `hosted`'s log before its effect, and the only root is the hosted
    /// client its binding names.
    pub(crate) fn new_hosted(
        registry: ParticipantRegistry,
        runners: Arc<BTreeMap<String, Arc<dyn VirtualAgent>>>,
        edges: Option<Arc<Mutex<EdgeLog>>>,
        policy: Arc<dyn CallPolicy>,
        hosted: Hosted,
    ) -> Self {
        Self {
            policy: Policy::Configured(policy),
            hosted: Some(hosted),
            ..Self::new(registry, runners, edges)
        }
    }

    /// The hosted state, on a hosted broker.
    pub(crate) fn hosted_state(&self) -> Option<&Hosted> {
        self.hosted.as_ref()
    }

    /// Raise `node`'s `human.ask` (EN-2b): stamp it with the name `node` was
    /// admitted under and the chain of the run it serves — whatever asker
    /// the worker put there is overwritten, and nothing above changes it —
    /// and give it a new id. [`RaisedQuestion::answers`] then relays it up
    /// the caller chain. `Err` for a node no broker call is running: nobody
    /// above it can answer.
    ///
    /// This and [`answer_question`](Self::answer_question) are the only ways
    /// a question enters and leaves the broker: the seam ADR-0023's gate
    /// (GT-0) puts its `human.ask` arms on — a `Node` raising one only for a
    /// call that is running it, the local `Root` answering only its own.
    pub(crate) fn raise_question(
        &self,
        node: &str,
        mut request: AskRequest,
    ) -> Result<RaisedQuestion, CallError> {
        // The gate first (ADR-0023 § 3): a node asks only from the task it
        // serves, whose run's chain stamps the asker.
        let peer = Peer::Node(node.to_string());
        let caller = self.resolve(&peer);
        let chain = match self.gate(&peer, &caller, &Request::Ask(&request)).outcome {
            Ok(Grant::Ask { chain }) => chain,
            Ok(other) => unreachable!("human.ask granted as {other:?}"),
            Err(refused) => return Err(refused.to_call_error()),
        };
        request.asker = Some(Asker {
            agent: node.to_string(),
            chain: chain.chain.clone(),
        });
        let id = format!(
            "question-{}",
            self.next_question.fetch_add(1, Ordering::Relaxed) + 1
        );
        info!(participant = %node, question = %id, "A node asked a question");
        Ok(RaisedQuestion {
            id,
            asker: node.to_string(),
            from: node.to_string(),
            request,
            registry: self.registry.clone(),
            routes: self.routes.clone(),
            swarm: self.swarm.clone(),
            questions: self.questions.clone(),
        })
    }

    /// The root's answers to question `id` (EN-2b), delivered to the
    /// request that asked. `Err` when no question waits on the root under
    /// `id`: it was answered, withdrawn, never raised, or waits on a node.
    pub fn answer_question(&self, id: &str, answers: Vec<Answer>) -> Result<(), CallError> {
        let mut questions = locked(&self.questions);
        if !questions
            .get(id)
            .is_some_and(|pending| matches!(pending.at, Hop::Root(_)))
        {
            return Err(CallError::Failed(format!(
                "no question '{id}' is waiting on the root"
            )));
        }
        let pending = questions.remove(id).expect("checked above");
        drop(questions);
        let _ = pending.reply.send(AskReply::Answers(answers));
        Ok(())
    }

    /// `node`'s reply to the question the broker relayed to it as `id`
    /// (EN-2b): its answers, or `escalate`. A reply for a question that
    /// does not wait on `node` — answered, withdrawn, relayed elsewhere — is
    /// dropped with a log line: the connection names who may answer what.
    pub(crate) fn question_reply(&self, node: &str, id: &str, reply: AskReply) {
        let mut questions = locked(&self.questions);
        if !questions
            .get(id)
            .is_some_and(|pending| matches!(&pending.at, Hop::Node(at) if at == node))
        {
            warn!(participant = %node, question = %id, "Dropping a reply to a question not relayed to this node");
            return;
        }
        let pending = questions.remove(id).expect("checked above");
        drop(questions);
        let _ = pending.reply.send(reply);
    }

    /// Raise `node`'s `human.approve` with the root (EN-2a): stamp it with
    /// the name `node` was admitted under and the chain of the run it
    /// serves — whatever asker the worker put there is overwritten — and
    /// deliver it, under a new id, to the root call that run is nested
    /// under. A node with no run, or whose root call has ended, is denied
    /// without asking.
    pub(crate) fn raise_approval(
        &self,
        node: &str,
        mut request: ApprovalRequest,
    ) -> RaisedApproval {
        // The gate first (ADR-0023 § 3): a refused approval is denied
        // without asking.
        let peer = Peer::Node(node.to_string());
        let caller = self.resolve(&peer);
        let chain = match self
            .gate(&peer, &caller, &Request::Approve(&request))
            .outcome
        {
            Ok(Grant::Approve { chain }) => chain,
            Ok(other) => unreachable!("human.approve granted as {other:?}"),
            Err(_) => return RaisedApproval::denied(&self.approvals),
        };
        let Some(root) = locked(&self.swarm).get(&chain.root_task_id).cloned() else {
            warn!(participant = %node, "Denying an approval with no root call to ask");
            return RaisedApproval::denied(&self.approvals);
        };
        request.asker = Some(Asker {
            agent: node.to_string(),
            chain: chain.chain.clone(),
        });
        let id = format!(
            "approval-{}",
            self.next_approval.fetch_add(1, Ordering::Relaxed) + 1
        );
        let (answer, verdict) = oneshot::channel();
        // In the table before it is sent, so a root call that ends in
        // between denies it.
        locked(&self.approvals).insert(
            id.clone(),
            PendingApproval {
                root_task_id: chain.root_task_id.clone(),
                answer,
                root: root.clone(),
            },
        );
        info!(participant = %node, approval = %id, kind = ?request.kind, "A node asked the root for an approval");
        if root
            .send(ToRoot::Approve {
                id: id.clone(),
                request,
            })
            .is_err()
        {
            locked(&self.approvals).remove(&id);
            return RaisedApproval::denied(&self.approvals);
        }
        RaisedApproval {
            pending: Some((id, verdict)),
            approvals: self.approvals.clone(),
        }
    }

    /// The root's answer to approval `id` (EN-2a), delivered to the request
    /// that asked. `Err` when nothing is waiting under `id`: it was
    /// answered, withdrawn, or never raised.
    pub fn answer_approval(&self, id: &str, verdict: ApprovalVerdict) -> Result<(), CallError> {
        let pending = locked(&self.approvals)
            .remove(id)
            .ok_or_else(|| CallError::Failed(format!("no approval '{id}' is pending")))?;
        let _ = pending.answer.send(verdict);
        Ok(())
    }

    /// Stop `node` and everything under it (TB-7, AGE-749): the call that
    /// runs it ends with a [`CANCELLED_BY_USER`] result, and its caller
    /// carries on. `node` is the name the broker admitted it under, or the
    /// spec the root addressed one of its own callees by — the name its
    /// caller's tree knows it by until the call ends. Every root callee of
    /// that spec is stopped then. `Err` when nothing running goes by `node`.
    pub fn cancel(&self, node: &str) -> Result<(), CallError> {
        let mut stops = locked(&self.stops);
        let by_node = stops.iter().any(|entry| entry.node == node);
        let (stopped, running): (Vec<_>, Vec<_>) = stops.drain(..).partition(|entry| {
            if by_node {
                entry.node == node
            } else {
                entry.by_root && entry.agent == node
            }
        });
        *stops = running;
        drop(stops);
        if stopped.is_empty() {
            return Err(CallError::Failed(format!(
                "nothing running is named '{node}'"
            )));
        }
        for entry in stopped {
            info!(node = %entry.node, "The user stopped a run");
            let _ = entry.stop.send(Stop::default());
        }
        Ok(())
    }

    /// Stop the calls running the tasks `levels` names, one level after
    /// another, without waiting for any of them (ADR-0092): what a caller
    /// that hung up does to the runs under the task it dropped.
    pub(crate) fn stop_now(&self, levels: &[Vec<String>]) {
        for level in levels {
            for entry in take_stops(&self.stops, level) {
                debug!(node = %entry.node, "Stopping a run whose caller's task was cancelled");
                let _ = entry.stop.send(Stop::default());
            }
        }
    }

    /// Log a refusal of a loopback HTTP request naming `what` — a role, a
    /// node, a handle or the swarm directory (BI-7). Nothing is on the tree
    /// yet for a squatting attempt like this, so `from` is a description of
    /// the peer, as `edge_log_schema_golden` already establishes for a
    /// refusal that never became a node.
    pub(crate) fn log_loopback_refusal(&self, what: &str) {
        let Some(log) = self.edges.as_ref() else {
            return;
        };
        let row = EdgeRow {
            ts: EdgeRow::now_ms(),
            kind: EdgeKind::Refusal,
            from: "loopback caller".to_string(),
            to: what.to_string(),
            scope: None,
            run: None,
            chain: Vec::new(),
            bytes: 0,
            outcome: "refused: fabric: roles are reached over the worker connection".to_string(),
            usd: None,
            root_task_id: None,
            names_sibling: None,
            sibling_names: None,
        };
        let mut log = log.lock().unwrap_or_else(|e| e.into_inner());
        if let Err(error) = log.append(&row) {
            warn!(%error, "Could not write a loopback-refusal edge-log row");
        }
    }

    /// Check every call, the root's included, against `policy` before
    /// anything is spawned. `None` is [`LocalPermissive`], by name.
    pub fn with_policy(mut self, policy: Option<Arc<dyn CallPolicy>>) -> Self {
        self.policy = match policy {
            Some(policy) => Policy::Configured(policy),
            None => Policy::LocalPermissive(LocalPermissive::new()),
        };
        self
    }

    /// Decide one of the local root's own requests (ADR-0023 § 3's local
    /// root row) before its effect.
    fn root_request(&self, request: &Request<'_>) -> Result<(), CallError> {
        self.root_request_as(&Caller::Root, request)
    }

    /// Decide one of the root's own requests as `caller`: the local root,
    /// or a hosted broker's client (HS-4a).
    pub(crate) fn root_request_as(
        &self,
        caller: &Caller,
        request: &Request<'_>,
    ) -> Result<(), CallError> {
        self.gate(&Peer::Root, caller, request)
            .outcome
            .map(|_| ())
            .map_err(|refused| refused.to_call_error())
    }

    /// Price each callee's reported usage on its task row with `pricer`.
    /// How often a call queued on a handle sends its caller a heartbeat
    /// (RC-3); `None` keeps [`QUEUED_HEARTBEAT`].
    pub fn with_queued_heartbeat(mut self, every: Option<std::time::Duration>) -> Self {
        if let Some(every) = every.filter(|every| !every.is_zero()) {
            self.queued_heartbeat = every;
        }
        self
    }

    pub fn with_pricer(mut self, pricer: Option<Arc<dyn UsagePricer>>) -> Self {
        self.pricer = pricer;
        self
    }

    /// Run `request` as `peer`. The stream is the call: progress, then one
    /// result or error. Dropping it cancels whatever the call started.
    ///
    /// Decided first, here and before anything else (ADR-0023 § 1): a
    /// refused call spawns, submits and queues nothing, and does not touch
    /// the caller's permit either (PL-S2).
    pub fn call(&self, peer: Peer, request: CallRequest) -> CallStream {
        self.call_in(peer, None, request)
    }

    /// [`call`](Self::call) from the root of the conversation `scope`
    /// (RC-4): its handles are that conversation's. `None` is the broker's
    /// one default conversation.
    pub fn call_in(
        &self,
        peer: Peer,
        scope: Option<&ConversationScope>,
        request: CallRequest,
    ) -> CallStream {
        let caller = self.resolve(&peer);
        self.call_as_in(peer, scope, caller, request)
    }

    /// Run `request` as `caller`, whom `peer`'s transport authenticated:
    /// [`call`](Self::call) for a peer the broker resolves itself, and a
    /// hosted broker's client (HS-4a), which its API resolved.
    pub(crate) fn call_as(&self, peer: Peer, caller: Caller, request: CallRequest) -> CallStream {
        self.call_as_in(peer, None, caller, request)
    }

    fn call_as_in(
        &self,
        peer: Peer,
        scope: Option<&ConversationScope>,
        caller: Caller,
        request: CallRequest,
    ) -> CallStream {
        match request {
            CallRequest::InvokeAgent(params) => {
                let (decision, checked) =
                    self.gate_checked(&peer, scope, &caller, &Request::Invoke(&params));
                let mut edge = EdgeGuard {
                    log: self.edges.clone(),
                    from: peer.name().to_string(),
                    to: params.agent.clone(),
                    chain: calling_chain(&caller, &params),
                    bytes: params.prompt.len() as u64,
                    outcome: None,
                    usd: None,
                    root_task_id: None,
                    sibling_names: None,
                };
                let (target, stamp) = match decision.outcome {
                    Ok(Grant::Invoke { target, stamp }) => (target, stamp),
                    Ok(other) => unreachable!("agent.invoke granted as {other:?}"),
                    Err(refused) => {
                        edge.refused(&refusal_row(&refused));
                        return refusal_stream(refused.to_call_error());
                    }
                };
                edge.root_task_id = Some(stamp.chain.root_task_id.clone());
                // A root call names the conversation its runs work for
                // (RC-4); a node's is its run's.
                let conversation = self.conversation_of(&peer, scope, params.run.as_deref());
                let root = match &peer {
                    Peer::Root => {
                        let root_task_id = stamp.chain.root_task_id.clone();
                        locked(&self.scopes).insert(root_task_id.clone(), conversation.clone());
                        Some(ScopeGuard {
                            scopes: self.scopes.clone(),
                            root_task_id,
                        })
                    }
                    Peer::Node(_) => None,
                };
                // The call joins the handle it names as the gate read it
                // (RC-4): a free name becomes a handle its caller owns.
                let ticket = match (checked, &target) {
                    (
                        HandleCheck::Named {
                            name,
                            status: expected,
                            ..
                        },
                        InvokeTarget::Spawn { .. },
                    ) => {
                        let key = HandleKey::new(conversation.clone(), name.as_str());
                        let owner = self.owner_line(&peer, &conversation);
                        match self.handles.begin(key, owner, &expected) {
                            Ok(ticket) => Some(ticket),
                            Err(_) => {
                                let error = CallError::Refused(format!(
                                    "handle '{name}' changed while the call was decided; call again"
                                ));
                                edge.refused(&error.to_string());
                                return refusal_stream(error);
                            }
                        }
                    }
                    _ => None,
                };
                let handle = CallHandle {
                    scope: conversation,
                    ticket,
                    root,
                };
                // Released now, before the callee queues for a slot that
                // may be this very one.
                let child = match &peer {
                    Peer::Node(name) => self.registry.node_permit(name).map(|p| p.child_call()),
                    Peer::Root => None,
                };
                let call = self.invoke(peer, params, edge, stamp, target, handle);
                match child {
                    Some(child) => gated(child, call),
                    None => call,
                }
            }
            CallRequest::ListAgents => match self.gate(&peer, &caller, &Request::List).outcome {
                Ok(_) => futures::stream::iter([Ok(CallEvent::Result(CallResult::Agents(
                    self.directory(),
                )))])
                .boxed(),
                Err(refused) => {
                    let error = refused.to_call_error();
                    self.log_refusal(&peer, "agent.list", &error.to_string());
                    refusal_stream(error)
                }
            },
            CallRequest::SendMessage(params) => {
                let status = self.send_message(&peer, &caller, params);
                futures::stream::iter([Ok(CallEvent::Result(CallResult::Posted(status)))]).boxed()
            }
            CallRequest::TakeMessages => match self.gate(&peer, &caller, &Request::Take).outcome {
                Ok(_) => {
                    let messages = self.take_mid_run(&peer);
                    futures::stream::iter([Ok(CallEvent::Result(CallResult::Messages(messages)))])
                        .boxed()
                }
                Err(refused) => {
                    let error = refused.to_call_error();
                    self.log_refusal(&peer, "mailbox.take", &error.to_string());
                    refusal_stream(error)
                }
            },
        }
    }

    /// The typed caller `peer` is for this request (ADR-0023 § 2): the
    /// root, or the node with the runs it serves — a node with none, or a
    /// name no node was admitted under, is a chainless `External`.
    fn resolve(&self, peer: &Peer) -> Caller {
        let Peer::Node(name) = peer else {
            return Caller::Root;
        };
        match self.registry.open_runs_of(name) {
            Some((spec, runs)) if !runs.is_empty() => Caller::Node(NodeCaller {
                name: name.clone(),
                spec,
                runs,
            }),
            _ => Caller::External(Admitter::Chainless),
        }
    }

    /// The root conversation `peer` calls for (RC-4): the root's own —
    /// `scope`, or the broker's default one — or, for a node, the one its
    /// run `run` hangs from. A run no root call in flight started gets a
    /// conversation of its own, which shares nothing.
    fn conversation_of(
        &self,
        peer: &Peer,
        scope: Option<&ConversationScope>,
        run: Option<&str>,
    ) -> ConversationScope {
        let name = match peer {
            Peer::Root => {
                return scope
                    .cloned()
                    .unwrap_or_else(|| ConversationScope::new(ROOT_SCOPE));
            }
            Peer::Node(name) => name,
        };
        let root_task_id = self.registry.open_runs_of(name).and_then(|(_, runs)| {
            runs.into_iter()
                .find(|open| Some(open.name.as_str()) == run)
                .map(|open| open.chain.root_task_id)
        });
        match root_task_id {
            Some(id) => locked(&self.scopes)
                .get(&id)
                .cloned()
                .unwrap_or_else(|| ConversationScope::new(format!("run:{id}"))),
            None => ConversationScope::new(format!("node:{name}")),
        }
    }

    /// Who `peer` is to a handle of `scope` (RC-4): the root, the handle
    /// whose task it runs, or itself.
    fn handle_party(&self, peer: &Peer, scope: &ConversationScope) -> HandleParty {
        match peer {
            Peer::Root => HandleParty::Root,
            Peer::Node(name) => match self.handles.served_by(name) {
                Some(key) if key.scope == *scope => HandleParty::Handle(key.name),
                _ => HandleParty::Node(name.clone()),
            },
        }
    }

    /// The root the runs of this broker hang from (ADR-0023 § 2): the
    /// human's own on a local or hosted-root broker; on a typed-root
    /// broker, the `External` key's owner it was bound to, or a `Remote`
    /// peer's.
    fn run_root(&self) -> RunRoot {
        match self.hosted.as_ref().map(|hosted| &hosted.binding.root) {
            None | Some(BrokerRoot::Hosted) => RunRoot::Owner,
            Some(BrokerRoot::Typed {
                key_owner: Some(key),
            }) => RunRoot::External { key: key.clone() },
            Some(BrokerRoot::Typed { key_owner: None }) => RunRoot::Remote,
        }
    }

    /// The idle handle `name` that `peer` may message (TM-4): one of the
    /// conversations `peer` works in holds it live and idle with a saved
    /// conversation, and `peer` is its owner or one of the owner's
    /// ancestors ([`HandleOwner::admits`]). Only a node has handles to
    /// message.
    ///
    /// `run` is the run the post names. With it, only that run's scope is
    /// checked, so a node with runs in two conversations never reaches the
    /// other conversation's handle of the same name; one that is not an
    /// open run of `peer` reaches nothing. Without it (an older worker),
    /// the scopes are the union over every run `peer` has open.
    fn idle_handle_of(&self, peer: &Peer, name: &str, run: Option<&str>) -> Option<HandleKey> {
        let Peer::Node(node) = peer else {
            return None;
        };
        let runs = self
            .registry
            .open_runs_of(node)
            .map_or_else(Vec::new, |(_, runs)| runs);
        let run_scope = |run: &super::gate::OpenRun| {
            locked(&self.scopes)
                .get(&run.chain.root_task_id)
                .cloned()
                .unwrap_or_else(|| {
                    ConversationScope::new(format!("run:{}", run.chain.root_task_id))
                })
        };
        let scopes = match run {
            Some(named) => vec![run_scope(runs.iter().find(|open| open.name == named)?)],
            None => {
                let mut scopes = vec![ConversationScope::new(format!("node:{node}"))];
                if let Some(key) = self.handles.served_by(node) {
                    scopes.push(key.scope);
                }
                scopes.extend(runs.iter().map(run_scope));
                scopes
            }
        };
        let root = self.run_root();
        scopes.into_iter().find_map(|scope| {
            let key = HandleKey::new(scope, name);
            let HandleStatus::Live(owner) = self.handles.status(&key) else {
                return None;
            };
            let party = self.handle_party(peer, &key.scope);
            (owner.admits(&root, &party) && self.handles.is_idle(&key)).then_some(key)
        })
    }

    /// The owner of a handle `peer` starts in `scope`, followed by its
    /// ancestors (RC-4): a handle's worker by its handle's line, a node by
    /// the nodes that own it up to the root, each as the handle it runs
    /// for, if it runs for one.
    fn owner_line(&self, peer: &Peer, scope: &ConversationScope) -> HandleOwner {
        let root = self.run_root();
        let mut party = self.handle_party(peer, scope);
        let owner = party.clone();
        let mut ancestors = Vec::new();
        loop {
            let up = match &party {
                HandleParty::Root => break,
                HandleParty::Handle(name) => {
                    if let HandleStatus::Live(line) =
                        self.handles.status(&HandleKey::new(scope.clone(), name))
                    {
                        // A handle's line starts at its owner, not at
                        // itself: all of it is above this one.
                        ancestors.extend(line.line);
                    }
                    break;
                }
                HandleParty::Node(name) => match self.registry.node_and_owner(name) {
                    Some((_, Some(owner))) => {
                        self.handle_party(&Peer::Node(owner.name().as_str().to_string()), scope)
                    }
                    Some((_, None)) | None => HandleParty::Root,
                },
            };
            ancestors.push(up.clone());
            party = up;
        }
        HandleOwner::new(root, owner, ancestors)
    }

    /// What [`decide`](gate::decide) reads for `request` from `peer`: the
    /// policy, the clock, the callee and the sender's owner, copied out of
    /// the registry and the runners. [`Unreadable`] when the registry does
    /// not add up.
    fn snapshot(
        &self,
        peer: &Peer,
        scope: Option<&ConversationScope>,
        request: &Request<'_>,
    ) -> Result<Snapshot<'_>, Unreadable> {
        let callee = match request {
            Request::Invoke(params) => self.callee(peer, params)?,
            _ => Callee::None,
        };
        let owner = match (request, peer) {
            (Request::Post(_), Peer::Node(name)) => match self.registry.node_and_owner(name) {
                None => Owner::None,
                Some((_, None)) => Owner::Root,
                Some((_, Some(owner))) => Owner::Node {
                    id: owner.id(),
                    name: owner.name().as_str().to_string(),
                    ended: owner.state() == NodeState::Ended,
                },
            },
            _ => Owner::None,
        };
        let addressee = match request {
            Request::Post(params) => match self.registry.node_and_owner(&params.to) {
                None if self
                    .idle_handle_of(peer, &params.to, params.run.as_deref())
                    .is_some() =>
                {
                    Addressee::Handle
                }
                None => Addressee::None,
                Some((node, owner)) => Addressee::Node {
                    id: node.id(),
                    owner: owner.map(|owner| owner.name().as_str().to_string()),
                    ended: node.state() == NodeState::Ended,
                },
            },
            _ => Addressee::None,
        };
        let answer_nonce = match (request, self.hosted.as_ref()) {
            (Request::Answer { id, .. } | Request::AnswerApproval { id, .. }, Some(hosted)) => {
                hosted.nonce_of(id)
            }
            _ => None,
        };
        let named = match request {
            Request::Invoke(params) => params
                .handle
                .as_deref()
                .map(|name| (name, params.run.as_deref())),
            Request::DropHandle { name, run } => Some((*name, *run)),
            _ => None,
        };
        let handle = match named {
            None => HandleCheck::None,
            Some((name, run)) => {
                let conversation = self.conversation_of(peer, scope, run);
                HandleCheck::Named {
                    name: name.to_string(),
                    party: self.handle_party(peer, &conversation),
                    root: self.run_root(),
                    status: self.handles.status(&HandleKey::new(conversation, name)),
                }
            }
        };
        Ok(Snapshot {
            policy: self.policy.get(),
            now: std::time::SystemTime::now(),
            root_task_id: uuid::Uuid::new_v4().to_string(),
            callee,
            owner,
            addressee,
            binding: self.hosted.as_ref().map(|hosted| &hosted.binding),
            answer_nonce,
            handle,
        })
    }

    /// Who `params.agent` names: a registered participant (by its node
    /// name), a virtual agent, or nobody. A registered name wins.
    fn callee(&self, peer: &Peer, params: &InvokeAgentParams) -> Result<Callee, Unreadable> {
        if let Some(spec) = self.registry.registered_spec(&params.agent) {
            let spec = spec.ok_or_else(|| {
                Unreadable(format!(
                    "participant '{}' is registered with no admitted node",
                    params.agent
                ))
            })?;
            return Ok(Callee::Node {
                name: params.agent.clone(),
                spec,
            });
        }
        let Some(runner) = self.runners.get(&params.agent) else {
            return Ok(Callee::Unknown);
        };
        let target = Target::of(runner.as_ref());
        // A node no runner recorded a context for is the root's.
        let own = match peer {
            Peer::Node(name) => self.registry.node_context(name),
            Peer::Root => None,
        }
        .unwrap_or_else(|| spawn_context::root(&self.runners, &target));
        let inside_own_tree = match (
            params
                .spawn_context
                .as_ref()
                .and_then(|context| context.workspace_root.as_deref()),
            own.workspace_root.as_deref(),
        ) {
            (Some(root), Some(tree)) => spawn_context::lies_inside(root, tree),
            _ => false,
        };
        Ok(Callee::Runner(SpawnView {
            target,
            own,
            inside_own_tree,
        }))
    }

    /// Decide `request` from `caller` (who `peer` resolved to), and log the
    /// outcome with the typed caller and the row it matched, before any
    /// effect. An internal error is alarmed.
    fn gate(&self, peer: &Peer, caller: &Caller, request: &Request<'_>) -> Decision {
        self.gate_in(peer, None, caller, request)
    }

    /// [`gate`](Self::gate) for a root `peer` calling from the
    /// conversation `scope` (RC-4).
    fn gate_in(
        &self,
        peer: &Peer,
        scope: Option<&ConversationScope>,
        caller: &Caller,
        request: &Request<'_>,
    ) -> Decision {
        self.gate_checked(peer, scope, caller, request).0
    }

    /// [`gate_in`](Self::gate_in), with the handle the decision read: a
    /// call that joins a handle joins it as the gate saw it (RC-4).
    fn gate_checked(
        &self,
        peer: &Peer,
        scope: Option<&ConversationScope>,
        caller: &Caller,
        request: &Request<'_>,
    ) -> (Decision, HandleCheck) {
        let snapshot = self.snapshot(peer, scope, request);
        let handle = snapshot
            .as_ref()
            .map(|snapshot| snapshot.handle.clone())
            .unwrap_or(HandleCheck::None);
        let mut decision = gate::decide(caller, request, &snapshot);
        // A hosted broker writes every decision before its effect, and a
        // write that fails refuses (ADR-0023 § 7): an unlogged grant on a
        // hosted broker is an internal error. A local broker's log is the
        // tracing line below, which cannot fail.
        if let Some(hosted) = self.hosted.as_ref()
            && let Err(why) = hosted.record(caller, &decision)
        {
            decision.outcome = Err(Refused::Internal(format!(
                "the decision log write failed: {why}"
            )));
        }
        match &decision.outcome {
            Ok(_) => debug!(target: "chatty::gate", %caller, row = %decision.row, "granted"),
            Err(Refused::Internal(why)) => {
                error!(target: "chatty::gate", %caller, row = %decision.row, %why, "refused: internal")
            }
            Err(refused) => {
                warn!(target: "chatty::gate", %caller, row = %decision.row, refused = %refused.to_call_error(), "refused")
            }
        }
        (decision, handle)
    }

    /// Log one refusal row for a request that is not an edge between two
    /// nodes: `what` names what it asked for.
    fn log_refusal(&self, peer: &Peer, what: &str, outcome: &str) {
        EdgeGuard {
            log: self.edges.clone(),
            from: peer.name().to_string(),
            to: what.to_string(),
            chain: Vec::new(),
            bytes: 0,
            outcome: None,
            usd: None,
            root_task_id: None,
            sibling_names: None,
        }
        .refused(outcome);
    }

    /// Queue `params` for its recipient as `caller`, or refuse it, and log
    /// one message row either way.
    fn send_message(
        &self,
        peer: &Peer,
        caller: &Caller,
        params: SendMessageParams,
    ) -> MessageStatus {
        let bytes = params.text.len() as u64;
        let to = params.to.clone();
        let sibling_names = self.siblings_named(peer, &to, &params.text);
        let status = self.accept_message(peer, caller, params);
        let outcome = match &status {
            MessageStatus::Pending { .. } => "pending".to_string(),
            MessageStatus::Refused { reason } => format!("refused: {reason}"),
        };
        debug!(from = %peer.name(), %to, %outcome, "send_message");
        EdgeGuard {
            log: self.edges.clone(),
            from: peer.name().to_string(),
            to,
            chain: vec![peer.name().to_string()],
            bytes,
            outcome: None,
            usd: None,
            root_task_id: match caller {
                Caller::Node(node) => node.runs.first().map(|run| run.chain.root_task_id.clone()),
                _ => None,
            },
            sibling_names: Some(sibling_names),
        }
        .write(EdgeKind::Message, outcome);
        status
    }

    /// The siblings of `peer` that its message `text` to `to` names, if `to`
    /// is `peer`'s owner (TM-3): a relay request. Empty for any other
    /// recipient, for the root, and for a name no node was admitted under.
    fn siblings_named(&self, peer: &Peer, to: &str, text: &str) -> Vec<String> {
        let Peer::Node(name) = peer else {
            return Vec::new();
        };
        self.registry.relay_siblings_named(name, to, text)
    }

    /// The gate's recipient check, then the pending list's bounds, which
    /// are the effect's quota: taken with the push, or refused. The sender
    /// is who its connection says, and its owner is who the directory says:
    /// the message names only the recipient, and a name that is not the
    /// sender's owner — a sibling, the sender itself, a name nobody has, a
    /// node of another conversation — is not on the tree.
    fn accept_message(
        &self,
        peer: &Peer,
        caller: &Caller,
        params: SendMessageParams,
    ) -> MessageStatus {
        let refused = |reason| MessageStatus::Refused { reason };
        let granted = match self.gate(peer, caller, &Request::Post(&params)).outcome {
            Ok(granted) => granted,
            Err(Refused::Message(reason)) => return refused(reason),
            // Nothing else refuses a post; were it to, it is not on the
            // tree.
            Err(_) => return refused(RefusalReason::NotOnTree),
        };
        let recipient = match granted {
            Grant::Post {
                to: PostTo::Root, ..
            } => Some(Recipient::Root),
            Grant::Post {
                to: PostTo::Node(id),
                at: Delivery::Run,
            } => Some(Recipient::Node(id)),
            Grant::Post {
                to: PostTo::Node(id),
                at: Delivery::ToolRound,
            } => Some(Recipient::MidRun(id)),
            // The sender's idle handle (TM-4), named by the sender and
            // the message.
            Grant::Post {
                to: PostTo::Handle, ..
            } => None,
            other => unreachable!("mailbox.post granted as {other:?}"),
        };
        let from = match peer {
            Peer::Root => Sender::Root,
            Peer::Node(name) => match self.registry.node_and_owner(name) {
                Some((sender, _)) => Sender::Node(sender.id()),
                None => return refused(RefusalReason::NotOnTree),
            },
        };

        let id = format!(
            "msg-{}",
            self.next_message.fetch_add(1, Ordering::Relaxed) + 1
        );
        let message = Message {
            id: id.clone(),
            from,
            from_name: peer.name().to_string(),
            text: params.text,
        };
        let pushed = match recipient {
            Some(recipient) => locked(&self.pending)
                .entry(recipient)
                .or_default()
                .push(message),
            None => {
                // The check and the push are one step under the pending
                // lock, so a clear (which needs it) cannot land between
                // them and leave the message on a freed key. Lock order:
                // `handle_pending`, then the handle store's and the
                // registry's locks (taken by `idle_handle_of`); nothing
                // takes them the other way round, and nothing awaits here.
                let mut pending = locked(&self.handle_pending);
                match self.idle_handle_of(peer, &params.to, params.run.as_deref()) {
                    Some(key) => {
                        #[cfg(test)]
                        if let Some(hook) = locked(&self.before_handle_push).as_ref() {
                            hook();
                        }
                        pending.entry(key).or_default().push(message)
                    }
                    None => Err(RefusalReason::NotOnTree),
                }
            }
        };
        match pushed {
            Ok(()) => MessageStatus::Pending { id },
            Err(reason) => refused(reason),
        }
    }

    /// Whose pending list `caller` reads: the root's, or its node's. `None`
    /// for a name no node was admitted under, which has nothing waiting.
    fn inbox(&self, caller: &Peer) -> Option<Recipient> {
        match caller {
            Peer::Root => Some(Recipient::Root),
            Peer::Node(name) => self
                .registry
                .node_and_owner(name)
                .map(|(node, _)| Recipient::Node(node.id())),
        }
    }

    /// `caller` is starting a new run — the root's next user turn, a node's
    /// next task: take what is waiting for it, wrapped and oldest first,
    /// and give each sender its allowance back (delivery point b).
    ///
    /// A node's run also opens with what its mid-run list still holds: the
    /// human's or its owner's messages that no tool round of its last run
    /// took (TM-5).
    pub fn start_run(&self, caller: &Peer) -> Vec<String> {
        let Some(inbox) = self.inbox(caller) else {
            return Vec::new();
        };
        let mid_run = match inbox {
            Recipient::Node(id) => Some(Recipient::MidRun(id)),
            Recipient::Root | Recipient::MidRun(_) => None,
        };
        let mut pending = locked(&self.pending);
        [Some(inbox), mid_run]
            .into_iter()
            .flatten()
            .flat_map(|inbox| {
                pending
                    .get_mut(&inbox)
                    .map(|list| {
                        list.start_run();
                        deliver(list)
                    })
                    .unwrap_or_default()
            })
            .collect()
    }

    /// The node `caller` is between two tool rounds (TM-5): take its
    /// mid-run list, wrapped and oldest first, one `delivered_mid_run`
    /// message row per message. The senders' allowances for this run stay
    /// spent.
    fn take_mid_run(&self, caller: &Peer) -> Vec<String> {
        let Some(Recipient::Node(id)) = self.inbox(caller) else {
            return Vec::new();
        };
        let taken = locked(&self.pending)
            .get_mut(&Recipient::MidRun(id))
            .map(PendingList::take_all)
            .unwrap_or_default();
        for message in &taken {
            debug!(from = %message.from_name, to = %caller.name(), id = %message.id, "Delivered a message mid-run");
            EdgeGuard {
                log: self.edges.clone(),
                from: message.from_name.clone(),
                to: caller.name().to_string(),
                chain: vec![message.from_name.clone()],
                bytes: message.bytes() as u64,
                outcome: None,
                usd: None,
                root_task_id: None,
                sibling_names: None,
            }
            .write(EdgeKind::Message, "delivered_mid_run".to_string());
        }
        taken.iter().map(Message::wrapped).collect()
    }

    /// The node `id`, admitted as `name`, has ended: what was waiting for it
    /// is dropped, one `message` row per message with outcome `dropped`.
    /// Later messages to it are `recipient_ended`.
    pub(crate) fn recipient_ended(&self, id: NodeId, name: &str) {
        let dropped: Vec<Message> = {
            let mut pending = locked(&self.pending);
            [Recipient::Node(id), Recipient::MidRun(id)]
                .iter()
                .filter_map(|inbox| pending.remove(inbox))
                .flat_map(|mut list| list.take_all())
                .collect()
        };
        write_dropped(&self.edges, &dropped, name);
    }

    /// Every agent a call can address, as the aggregated agent card lists
    /// them: connected participants, then virtual agents, each with its
    /// origin (ADR-0011 C5).
    fn directory(&self) -> Vec<AgentEntry> {
        let participants = self
            .registry
            .agents()
            .into_iter()
            .map(|agent| AgentEntry::from_card(&agent.card, agent.origin));
        let runners = self
            .runners
            .values()
            .map(|runner| AgentEntry::from_card(&runner.agent_card(), AgentOrigin::Local));
        participants.chain(runners).collect()
    }

    /// Run a granted `agent.invoke` on the target its grant names — never
    /// a second lookup of the name it addressed (ADR-0023 § 1).
    fn invoke(
        &self,
        caller: Peer,
        params: InvokeAgentParams,
        mut edge: EdgeGuard,
        stamp: CallStamp,
        target: InvokeTarget,
        handle: CallHandle,
    ) -> CallStream {
        let registry = self.registry.clone();
        // A worker the call starts gets the context the gate derived or
        // clamped from the caller's own (BI-5).
        let (effect, spawn) = match target {
            InvokeTarget::Submit { node } => (Effect::Submit(node), None),
            InvokeTarget::Spawn { runner, context } => match self.runners.get(&runner) {
                Some(runner) => (Effect::Spawn(runner.clone()), Some(context)),
                None => {
                    unreachable!("the gate granted a spawn of '{runner}', which no runner serves")
                }
            },
        };
        // When the broker stops a callee that outran its deadline (DP-3).
        let left = stamp.chain.deadline.map(|deadline| {
            deadline
                .duration_since(std::time::SystemTime::now())
                .unwrap_or_default()
        });
        let cut = left.map(|left| tokio::time::Instant::now() + left + deadline_grace(left));
        // How long a stop or a deadline gives the runs under the callee to
        // go, from the moment it fires (ADR-0092).
        let grace = deadline_grace(left.unwrap_or_default());
        let pricer = self.pricer.clone();
        let stops = self.stops.clone();
        let routes = self.routes.clone();
        let questions = self.questions.clone();
        // A root call listens for the runs nested under it; a run a node's
        // call starts reports to its root call, if that is listening
        // (TB-1). Both are keyed by the chain the broker stamped.
        let chain = stamp.chain.clone();
        let route_chain = chain.clone();
        let mut listening = match &caller {
            Peer::Root => Some(Listening::open(
                &self.swarm,
                &self.approvals,
                &self.questions,
                &chain.root_task_id,
            )),
            Peer::Node(_) => None,
        };
        let reports_to = match &caller {
            Peer::Node(_) => locked(&self.swarm)
                .get(&chain.root_task_id)
                .cloned()
                .map(|to| (to, chain)),
            Peer::Root => None,
        };
        let mut task = DelegatedTask::new(params.prompt)
            .with_call(Some(stamp))
            .with_spawn_context(spawn)
            .with_swarm_events(reports_to.is_some())
            // Every run hands back its conversation, so the root can export
            // the whole tree (AGE-859): a nested run's goes to the root as a
            // swarm item, the root's callee's rides its result.
            .with_capture_conversation(true)
            // A hosted broker's tasks carry its binding, and no other
            // (ADR-0021 § 3).
            .with_identity(self.hosted.as_ref().map(Hosted::identity))
            // Which of the caller's files the worker gets (AGE-1735): the
            // runner applies it when it starts the worker.
            .with_files(params.files);
        let agent = params.agent;
        // A call naming a handle (RC-3) queues on it; the gate decided it
        // may (RC-4).
        let handle_name = params.handle;
        let CallHandle {
            scope,
            ticket,
            root: _root_scope,
        } = handle;
        let handles = self.handles.clone();
        let handle_pending = self.handle_pending.clone();
        let edge_log = self.edges.clone();
        let submits = matches!(effect, Effect::Submit(_));
        let queued_heartbeat = self.queued_heartbeat;
        let tasks = self.tasks.clone();
        let clock = self.clock.clone();
        let caller_name = caller.name().to_string();
        // The caller's messages ride on this call's result (delivery point
        // a), taken when the result is made.
        let inbox = self
            .inbox(&caller)
            .map(|inbox| (inbox, self.pending.clone()));
        let messages = move || match &inbox {
            Some((inbox, pending)) => locked(pending)
                .get_mut(inbox)
                .map(deliver)
                .unwrap_or_default(),
            None => Vec::new(),
        };

        async_stream::stream! {
            // Calls to one handle run one at a time, in arrival order: the
            // slot is held until this call's stream is dropped, after its
            // worker is reaped (RC-3).
            // A call that has to wait says so once, then heartbeats, so its
            // caller's stall watchdog does not end the turn under it; one
            // that gets the slot at once sends nothing.
            // The root call's entry in the scopes table lives as long as
            // this stream.
            let _root_scope = _root_scope;
            let held = match (ticket.as_ref(), handle_name.as_deref()) {
                (Some(ticket), Some(name)) => {
                    let acquire = ticket.queue().lock_owned();
                    tokio::pin!(acquire);
                    let mut beat = tokio::time::interval(queued_heartbeat);
                    beat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                    let mut beats = 0u64;
                    loop {
                        tokio::select! {
                            biased;
                            held = &mut acquire => break Some(held),
                            _ = beat.tick() => {
                                // The first tick is immediate: only a call
                                // the slot did not let in at once gets here.
                                beats += 1;
                                yield Ok(CallEvent::Progress(if beats == 1 {
                                    WireProgress::Step(format!(
                                        "waiting for handle '{name}': another call to it is running"
                                    ))
                                } else {
                                    WireProgress::heartbeat()
                                }));
                            }
                        }
                    }
                }
                _ => None,
            };
            // What the handle holds, now that this call is first in line:
            // a handle that ended while the call waited spawns nothing.
            let saved = match ticket.as_ref().map(HandleTicket::saved) {
                Some(Ok(saved)) => saved,
                Some(Err(error)) => {
                    warn!(agent = %agent, %error, "A queued handle call found its handle ended");
                    edge.root_task_id = None;
                    edge.refused(&error.to_string());
                    yield Err(CallError::Handle(error));
                    return;
                }
                None => None,
            };
            let mut undelivered = Undelivered {
                edges: edge_log.clone(),
                to: handle_name.clone().unwrap_or_default(),
                messages: Vec::new(),
            };
            if let Some(name) = handle_name.as_deref() {
                let refused = if submits {
                    Some(format!(
                        "handle '{name}': only an agent the broker starts can be resumed, not \
                         the running participant '{agent}'"
                    ))
                } else {
                    match saved.as_ref() {
                        Some(saved) if saved.agent != agent => Some(format!(
                            "handle '{name}' holds a conversation with '{}', not '{agent}'",
                            saved.agent
                        )),
                        // A resumed worker keeps the files its first task
                        // got: a later call's `files` would be silently
                        // ignored, so it is refused (AGE-1735).
                        Some(_) if task.files.is_some() => Some(format!(
                            "handle '{name}': `files` can only narrow a new sub-agent's \
                             inputs; a continuing handle keeps its first files"
                        )),
                        Some(saved) => match Opaque::from_value(&saved.conversation.data) {
                            Ok(conversation) => {
                                task = task
                                    .with_resume(Some(conversation))
                                    .with_attach(saved.tree.clone());
                                // The owner's messages to this handle open
                                // the task, as a node's own open its next
                                // run (TM-4, delivery point b).
                                let waiting = start_handle_run(
                                    &handle_pending,
                                    &HandleKey::new(scope.clone(), name),
                                );
                                if !waiting.is_empty() {
                                    let wrapped: Vec<String> =
                                        waiting.iter().map(Message::wrapped).collect();
                                    task.text = format!("{}\n\n{}", wrapped.join("\n"), task.text);
                                    undelivered.messages = waiting;
                                }
                                None
                            }
                            Err(e) => Some(format!("handle '{name}' cannot be resumed: {e}")),
                        },
                        None => None,
                    }
                };
                if let Some(reason) = refused {
                    warn!(agent = %agent, %reason, "Refusing a handle call");
                    // No worker was started: the row must not count as one
                    // (TM-3).
                    edge.root_task_id = None;
                    edge.refused(&reason);
                    yield Ok(CallEvent::Result(outcome(TaskState::Failed, String::new(), Some(reason), None, messages())));
                    return;
                }
            }
            // A connected participant is already running with the files it
            // has: the task frame has no room for `files`, so narrowing them
            // would be silently ignored, and is refused (AGE-1735).
            if submits && task.files.is_some() {
                let reason = format!(
                    "participant '{agent}' is already running: `files` can only narrow a new \
                     sub-agent's inputs; a connected participant keeps the files it has"
                );
                warn!(agent = %agent, %reason, "Refusing a call narrowing a running participant's files");
                // No worker was started: the row must not count as one
                // (TM-3).
                edge.root_task_id = None;
                edge.refused(&reason);
                yield Ok(CallEvent::Result(outcome(TaskState::Failed, String::new(), Some(reason), None, messages())));
                return;
            }
            let running = match effect {
                Effect::Submit(node) => a2a_participant::submit(&registry, &node, task).await
                    .ok_or_else(|| format!("participant '{agent}' is no longer connected")),
                // A worker that never started is a setup problem — its
                // worktree, its process, its hello — that only the user can
                // fix, so it is typed for every caller up the tree to stop
                // on and the user to be shown (AGE-822).
                Effect::Spawn(runner) => {
                    info!(caller = %caller.name(), agent = %agent, "Starting a worker for a call");
                    a2a_participant::spawn(runner.as_ref(), task)
                        .await
                        .map_err(|reason| chatty_fabric::worker_start_failed(&agent, &reason))
                }
            };
            let mut running = match running {
                Ok(running) => running,
                Err(reason) => {
                    warn!(agent = %agent, %reason, "Could not start a worker for a call");
                    // `to` is still the spec here, not a node: a row naming
                    // no worker must not make the run look multi-worker
                    // (TM-3).
                    edge.root_task_id = None;
                    edge.end(TaskState::Failed);
                    yield Ok(CallEvent::Result(outcome(TaskState::Failed, String::new(), Some(reason), None, messages())));
                    return;
                }
            };
            // The worker has the messages in its task now.
            undelivered.disarm();
            edge.to = running.participant().to_string();
            // The worker of a handle's task calls as that handle; a plain
            // call's spawned worker owns what handles it starts until the
            // call ends (RC-4).
            if let Some(ticket) = ticket.as_ref() {
                ticket.served_by(running.participant());
            }
            let _owned = (!submits && ticket.is_none()).then(|| OwnerGuard {
                handles: handles.clone(),
                handle_pending: handle_pending.clone(),
                edges: edge_log.clone(),
                scope: scope.clone(),
                node: running.participant().to_string(),
            });
            // `tasks/get` answers for it from now on (RC-4).
            locked(&tasks).start(
                running.task_id(),
                TaskRecord::new(scope.clone(), &caller_name, &agent, handle_name.clone()),
            );
            // A call dropped before its task ended leaves it cancelled.
            let _task_record = TaskEnd {
                tasks: tasks.clone(),
                id: running.task_id().to_string(),
                clock: clock.clone(),
            };
            // Where the callee's questions go while this call runs it
            // (EN-2b): before anything is yielded, so a question the worker
            // asks at once finds it.
            let call = NEXT_STOPPABLE.fetch_add(1, Ordering::Relaxed);
            locked(&routes).insert(
                running.participant().to_string(),
                Route { call, caller: caller.clone(), chain: route_chain },
            );
            let _route = RouteGuard {
                routes: routes.clone(),
                questions: questions.clone(),
                registry: registry.clone(),
                node: running.participant().to_string(),
                call,
            };
            // As soon as admitted, before anything else: a caller that
            // only knew this callee by its spec can now name this one
            // call precisely (AGE-762), which two parallel calls to the
            // same spec need to be stoppable one at a time.
            // Stoppable before anything is yielded, so a cancel above it
            // that comes now finds it (ADR-0092).
            let (mut stopped, _stoppable) =
                stoppable(&stops, &caller, &agent, running.participant(), running.task_id());
            yield Ok(CallEvent::Progress(WireProgress::Admitted(running.participant().to_string())));
            let mut stopped_by_user = false;
            let mut cut_off = false;
            let mut reaped = None;
            let reporting = reports_to.map(|(to, chain)| Reporting {
                to,
                chain,
                node: running.participant().to_string(),
                ended: AtomicBool::new(false),
            });
            let report = |item: SwarmItem| {
                if let Some(reporting) = reporting.as_ref() {
                    reporting.send(item);
                }
            };
            // The first flush is one interval in, so a node's batches are
            // an interval apart from the call's start on.
            let mut batcher = SwarmBatcher::new();
            let mut flush = tokio::time::interval_at(
                tokio::time::Instant::now() + FORWARD_INTERVAL,
                FORWARD_INTERVAL,
            );
            flush.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

            let mut response = String::new();
            let mut end = None;
            loop {
                // Biased, so the callee's updates go out in the order it
                // sent them, ahead of a question or an approval it raised
                // after them (EN-2b); a stop or a deadline goes first.
                let update = tokio::select! {
                    biased;
                    _ = sleep_until_cut(cut) => {
                        warn!(agent = %agent, "A called task ran past its deadline; stopping it");
                        cut_off = true;
                        end = Some((
                            TaskState::Failed,
                            Some("deadline: the call ran past its deadline and was stopped".to_string()),
                            None,
                        ));
                        break;
                    }
                    Ok(stop) = &mut stopped => {
                        stopped_by_user = true;
                        reaped = stop.reaped;
                        break;
                    }
                    update = running.updates.recv() => match update {
                        Some(update) => update,
                        None => break,
                    },
                    Some(to_root) = next_nested(&mut listening) => {
                        match to_root {
                            ToRoot::Nested(nested) => {
                                batcher.push(&nested.node, &nested.chain, nested.item);
                            }
                            ToRoot::Approve { id, request } => {
                                yield Ok(CallEvent::Approve { id, request });
                            }
                            ToRoot::Ask { id, request } => {
                                yield Ok(CallEvent::Ask { id, request });
                            }
                            ToRoot::Withdrawn { id } => {
                                yield Ok(CallEvent::InputWithdrawn { id });
                            }
                        }
                        continue;
                    }
                    _ = flush.tick(), if !batcher.is_empty() => {
                        for batch in batcher.flush() {
                            yield Ok(CallEvent::Swarm(batch));
                        }
                        continue;
                    }
                };
                match update {
                    TaskUpdate::Artifact { text, .. } => {
                        if !text.is_empty() {
                            report(SwarmItem::Text { bytes: text.len() as u64 });
                            response.push_str(&text);
                            yield Ok(CallEvent::Progress(WireProgress::Text(text)));
                        }
                    }
                    TaskUpdate::Event(item) => {
                        if item.is_workers_to_report() {
                            report(item);
                        }
                    }
                    TaskUpdate::Status { state, message, metadata } => {
                        if state.is_terminal() {
                            if let Some(usage) = metadata.as_ref().and_then(|m| m.usage.as_ref())
                                && let Ok(usage) = serde_json::to_value(usage)
                            {
                                report(SwarmItem::Usage { usage });
                            }
                            if let Some(conversation) = metadata
                                .as_ref()
                                .and_then(CapturedConversation::from_metadata)
                            {
                                report(SwarmItem::Conversation { conversation });
                            }
                            // Finished before the result, as the A2A path
                            // does before its terminal event: that commits
                            // the worker's tree, and the evidence read off
                            // an uncommitted one is stale (AGE-406).
                            let evidence = running
                                .finish(state == TaskState::Completed, metadata.as_ref())
                                .await;
                            if let Some(evidence) = evidence.as_ref() {
                                response.push_str(&evidence.text);
                                yield Ok(CallEvent::Progress(WireProgress::Text(evidence.text.clone())));
                            }
                            end = Some((
                                state,
                                message,
                                a2a_participant::with_evidence(metadata, evidence.as_ref()),
                            ));
                            break;
                        }
                        if state == TaskState::Working {
                            // A working status with no line is the worker's
                            // heartbeat (AGE-932): its model is busy, which
                            // the caller's stall watchdog must hear.
                            yield Ok(CallEvent::Progress(match message {
                                Some(step) => WireProgress::Step(step),
                                None => WireProgress::heartbeat(),
                            }));
                        }
                    }
                }
            }

            if stopped_by_user || cut_off {
                // The subtree goes first, deepest runs first, before this
                // call reports its end (ADR-0092).
                stop_subtree(
                    &stops,
                    &registry,
                    running.participant(),
                    running.task_id(),
                    tokio::time::Instant::now() + grace,
                )
                .await;
            }
            if stopped_by_user {
                // Then the callee itself, now, not when the caller lets go
                // of the stream: the worker is reaped, its connection
                // closes, and every call it made is dropped with it. A
                // spawned worker is waited for, so a stop above this one
                // knows this level is gone.
                locked(&tasks).end(running.task_id(), TaskState::Canceled, (clock)());
                let node = running.participant().to_string();
                drop(running);
                if !submits {
                    registry
                        .wait_gone(&node, tokio::time::Instant::now() + grace)
                        .await;
                }
                if let Some(reaped) = reaped.take() {
                    let _ = reaped.send(());
                }
                report(SwarmItem::Ended { state: TaskState::Canceled.to_string() });
                edge.write(EdgeKind::Task, "cancelled".to_string());
                yield Ok(CallEvent::Result(CallResult::Invoked(InvokeAgentOutcome {
                    success: false,
                    response,
                    error: Some(CANCELLED_BY_USER.to_string()),
                    metadata: None,
                    messages: messages(),
                    cancelled_by_user: true,
                    handle: None,
                    behind_by: None,
                })));
                return;
            }
            let (state, message, metadata) = end.unwrap_or_else(|| {
                debug!(agent = %agent, "A called task ended with no final status");
                (
                    TaskState::Failed,
                    Some("the participant ended the task without a final status".to_string()),
                    None,
                )
            });
            edge.usd = pricer
                .as_ref()
                .zip(metadata.as_ref().and_then(|m| m.usage.as_ref()))
                .and_then(|(pricer, usage)| pricer.usd(usage));
            report(SwarmItem::Ended { state: state.to_string() });
            // Every nested run ended before the callee did, so what they
            // reported is all here: it goes out, on the next flush, before
            // the result.
            // A withdrawal still queued goes out before the result; an
            // approval still pending is denied, and a question left
            // unanswered, as the call ends.
            if let Some(listening) = listening.as_mut() {
                while let Ok(to_root) = listening.nested.try_recv() {
                    match to_root {
                        ToRoot::Nested(nested) => {
                            batcher.push(&nested.node, &nested.chain, nested.item);
                        }
                        ToRoot::Withdrawn { id } => {
                            yield Ok(CallEvent::InputWithdrawn { id });
                        }
                        ToRoot::Approve { .. } | ToRoot::Ask { .. } => {}
                    }
                }
            }
            if !batcher.is_empty() {
                flush.tick().await;
                for batch in batcher.flush() {
                    yield Ok(CallEvent::Swarm(batch));
                }
            }
            // The handle keeps what this task said, after what its earlier
            // tasks said, and the tree it worked in (RC-3).
            let mut saved_as = None;
            let mut behind_by = None;
            if let (Some(ticket), Some(name)) = (ticket.as_ref(), handle_name.as_ref()) {
                let tree = running.tree();
                match metadata.as_ref().and_then(CapturedConversation::from_metadata) {
                    Some(CapturedConversation::Messages { messages: turns }) => {
                        let conversation = match saved.as_ref() {
                            Some(previous) => previous.conversation.extended(&turns),
                            None => SavedConversation::new(turns),
                        };
                        let kept = ticket.save(Handle {
                            name: name.clone(),
                            agent: agent.clone(),
                            conversation,
                            tree: tree
                                .as_ref()
                                .map(|info| info.tree.clone())
                                .or_else(|| saved.and_then(|previous| previous.tree)),
                            last_task_base: tree.as_ref().and_then(|info| info.task_base.clone()),
                        });
                        // A handle that ended while its task ran keeps
                        // nothing, and the result names none (RC-4).
                        if kept {
                            saved_as = Some(name.clone());
                            behind_by = tree.and_then(|info| info.behind_by);
                        }
                    }
                    // Nothing to resume from: the handle goes rather than
                    // resume a conversation that misses this task.
                    _ => {
                        warn!(agent = %agent, handle = %name, "The task captured no conversation; the handle is dropped");
                        ticket.clear();
                        drop_handle_mail(&handle_pending, &edge_log, ticket.key());
                    }
                }
            }
            locked(&tasks).end(running.task_id(), state, (clock)());
            let node = running.participant().to_string();
            // The worker is reaped before the handle's next call may start,
            // and the handle is idle from then: a tuple drops its fields in
            // order.
            let _reap_then_release = (running, _owned, held, ticket);
            if cut_off {
                // A callee stopped by its deadline is gone before its
                // caller hears so, like one the user stopped (ADR-0092).
                drop(_reap_then_release);
                if !submits {
                    registry
                        .wait_gone(&node, tokio::time::Instant::now() + grace)
                        .await;
                }
            }
            edge.end(state);
            let mut result = outcome(state, response, message, metadata, messages());
            if let CallResult::Invoked(invoked) = &mut result {
                invoked.handle = saved_as;
                invoked.behind_by = behind_by;
            }
            yield Ok(CallEvent::Result(result));
            // `running` is dropped here, which reaps a spawned worker, then
            // the handles it owned end, if it ran a plain call, and the
            // handle's queue is let go.
        }
        .boxed()
    }
}

/// `call` with its caller's permit re-acquired before its result (or error)
/// is delivered — at once unless it is the caller's last outstanding call.
///
/// The callee's stream is dropped before the wait: that reaps the callee's
/// worker and frees its permit, which a callee on the caller's endpoint is
/// holding.
fn gated(child: ChildCall, mut call: CallStream) -> CallStream {
    async_stream::stream! {
        while let Some(event) = call.next().await {
            if matches!(event, Ok(CallEvent::Result(_)) | Err(_)) {
                drop(call);
                child.finish().await;
                yield event;
                return;
            }
            yield event;
        }
        // No result at all: the connection reports that as a failed call,
        // which starts the caller's next model call just the same.
        drop(call);
        child.finish().await;
    }
    .boxed()
}

/// What a granted `agent.invoke` does: hand its task to the registered
/// participant it names, or spawn a worker of the runner it names.
enum Effect {
    Submit(String),
    Spawn(Arc<dyn VirtualAgent>),
}

/// A call that ends, refused, with `error`.
fn refusal_stream(error: CallError) -> CallStream {
    futures::stream::iter([Err(error)]).boxed()
}

/// The outcome a refused `agent.invoke`'s row records, as it did before the
/// gate: `unknown agent`, the delegation refusal, or the call's error.
fn refusal_row(refused: &Refused) -> String {
    match refused {
        Refused::UnknownAgent(_) => "unknown agent".to_string(),
        Refused::Delegation(refusal) => refusal.to_string(),
        Refused::Roster(_)
        | Refused::SpawnContext(_)
        | Refused::Message(_)
        | Refused::Handle(_)
        | Refused::Caller(_)
        | Refused::Internal(_) => refused.to_call_error().to_string(),
    }
}

/// The chain an `agent.invoke` is made from, as its row records it: the
/// root's, or the named run's. Empty for a call from no run of the
/// caller's.
fn calling_chain(caller: &Caller, params: &InvokeAgentParams) -> Vec<String> {
    match caller {
        Caller::Root | Caller::HostedRoot(_) => vec![ROOT_NAME.to_string()],
        Caller::Node(node) => node
            .runs
            .iter()
            .find(|run| Some(run.name.as_str()) == params.run.as_deref())
            .map(|run| run.chain.chain.clone())
            .unwrap_or_default(),
        Caller::Remote(_) | Caller::External(_) => Vec::new(),
    }
}

/// The result of an `agent.invoke` request whose task ended in `state`,
/// carrying the caller's waiting `messages`. Only a failure is a failure: a
/// task that was cancelled from its own side reads as it does to an A2A
/// caller.
fn outcome(
    state: TaskState,
    response: String,
    message: Option<String>,
    metadata: Option<TaskMetadata>,
    messages: Vec<String>,
) -> CallResult {
    let success = state != TaskState::Failed;
    CallResult::Invoked(InvokeAgentOutcome {
        success,
        response,
        error: if success { None } else { message },
        metadata: metadata.map(Box::new),
        messages,
        cancelled_by_user: false,
        handle: None,
        behind_by: None,
    })
}

/// Take everything waiting on `list`, as its recipient reads it.
fn deliver(list: &mut PendingList) -> Vec<String> {
    list.take_all().iter().map(Message::wrapped).collect()
}

/// A handle starts a task: take what was sent to it, oldest first, and give
/// the senders their allowance back (TM-4).
fn start_handle_run(
    pending: &Mutex<HashMap<HandleKey, PendingList>>,
    key: &HandleKey,
) -> Vec<Message> {
    locked(pending)
        .get_mut(key)
        .map(|list| {
            list.start_run();
            list.take_all()
        })
        .unwrap_or_default()
}

/// One `message` row with outcome `dropped` per message that will never
/// reach `to`.
fn write_dropped(edges: &Option<Arc<Mutex<EdgeLog>>>, dropped: &[Message], to: &str) {
    for message in dropped {
        debug!(from = %message.from_name, %to, id = %message.id, "Dropped a message with its recipient");
        EdgeGuard {
            log: edges.clone(),
            from: message.from_name.clone(),
            to: to.to_string(),
            chain: vec![message.from_name.clone()],
            bytes: message.bytes() as u64,
            outcome: None,
            usd: None,
            root_task_id: None,
            sibling_names: None,
        }
        .write(EdgeKind::Message, "dropped".to_string());
    }
}

/// The handle `key` is gone: what waited for it is dropped, with a row each.
fn drop_handle_mail(
    pending: &Mutex<HashMap<HandleKey, PendingList>>,
    edges: &Option<Arc<Mutex<EdgeLog>>>,
    key: &HandleKey,
) {
    let dropped = locked(pending)
        .remove(key)
        .map(|mut list| list.take_all())
        .unwrap_or_default();
    write_dropped(edges, &dropped, &key.name);
}

/// Drop what waits for every handle that is no longer live: ended by its
/// owner, its conversation, idleness or eviction.
fn drop_ended_handle_mail(
    handles: &HandleStore,
    pending: &Mutex<HashMap<HandleKey, PendingList>>,
    edges: &Option<Arc<Mutex<EdgeLog>>>,
) {
    let waiting: Vec<HandleKey> = locked(pending).keys().cloned().collect();
    for key in waiting {
        if !matches!(handles.status(&key), HandleStatus::Live(_)) {
            drop_handle_mail(pending, edges, &key);
        }
    }
}

/// Messages taken for a handle's task that has not started: unless
/// [`disarm`](Self::disarm)ed (the worker started), dropping this writes a
/// `dropped` row for each, as a spawn failure or a cancelled call leaves
/// them nowhere to go (TM-4).
struct Undelivered {
    edges: Option<Arc<Mutex<EdgeLog>>>,
    to: String,
    messages: Vec<Message>,
}

impl Undelivered {
    fn disarm(&mut self) {
        self.messages.clear();
    }
}

impl Drop for Undelivered {
    fn drop(&mut self) {
        write_dropped(&self.edges, &self.messages, &self.to);
    }
}

/// Writes a call's edge-log row when the call ends, however it ends: a call
/// whose stream is dropped before its result was cancelled.
struct EdgeGuard {
    log: Option<Arc<Mutex<EdgeLog>>>,
    from: String,
    to: String,
    /// The caller's chain, spec names root first (DP-2).
    chain: Vec<String>,
    bytes: u64,
    /// Set once the row is written.
    outcome: Option<String>,
    /// The callee's reported usage, priced (DP-3).
    usd: Option<String>,
    /// The run the row belongs to (DP-2).
    root_task_id: Option<String>,
    /// A message row's sibling names (TM-3); `None` on any other row.
    sibling_names: Option<Vec<String>>,
}

impl EdgeGuard {
    fn end(&mut self, state: TaskState) {
        self.write(EdgeKind::Task, state.to_string());
    }

    fn refused(&mut self, why: &str) {
        self.write(EdgeKind::Refusal, why.to_string());
    }

    fn write(&mut self, kind: EdgeKind, outcome: String) {
        if self.outcome.is_some() {
            return;
        }
        self.outcome = Some(outcome.clone());
        let Some(log) = self.log.as_ref() else {
            return;
        };
        let row = EdgeRow {
            ts: EdgeRow::now_ms(),
            kind,
            from: self.from.clone(),
            to: self.to.clone(),
            scope: Some(ConversationScope::new(ROOT_SCOPE)),
            run: None,
            chain: self.chain.clone(),
            bytes: self.bytes,
            outcome,
            usd: self.usd.clone(),
            root_task_id: self.root_task_id.clone(),
            names_sibling: self.sibling_names.as_ref().map(|names| !names.is_empty()),
            sibling_names: self.sibling_names.clone(),
        };
        let mut log = log.lock().unwrap_or_else(|e| e.into_inner());
        if let Err(error) = log.append(&row) {
            warn!(%error, "Could not write an edge-log row");
        }
    }
}

impl Drop for EdgeGuard {
    fn drop(&mut self) {
        if self.outcome.is_none() {
            self.write(EdgeKind::Task, TaskState::Canceled.to_string());
        }
    }
}

/// The in-process root's [`Transport`]: calls run straight on its broker,
/// with no socket and no HTTP hop.
pub struct DirectTransport {
    calls: Arc<BrokerCalls>,
    /// The root conversation calling through it (RC-4); `None` is the
    /// broker's one default conversation.
    scope: Option<ConversationScope>,
}

impl DirectTransport {
    pub fn new(calls: Arc<BrokerCalls>) -> Self {
        Self { calls, scope: None }
    }

    /// A2A `tasks/get` for this root's conversation (RC-4); see
    /// [`BrokerCalls::tasks_get`].
    pub fn tasks_get(&self, id: &str) -> Result<Option<serde_json::Value>, CallError> {
        self.calls.tasks_get(self.scope.as_ref(), id)
    }
}

#[async_trait::async_trait]
impl Transport for DirectTransport {
    async fn call(&self, req: CallRequest) -> Result<CallStream, CallError> {
        Ok(self.calls.call_in(Peer::Root, self.scope.as_ref(), req))
    }

    /// The same broker, for the root of the conversation `scope`: its
    /// handles are that conversation's alone (RC-4).
    fn for_conversation(&self, scope: &ConversationScope) -> Option<Arc<dyn Transport>> {
        Some(Arc::new(Self {
            calls: self.calls.clone(),
            scope: Some(scope.clone()),
        }))
    }

    /// The root's answers to a question the broker delivered (EN-2b).
    async fn answer(&self, id: &str, answers: Vec<Answer>) -> Result<(), CallError> {
        self.calls
            .root_request(&Request::Answer { id, nonce: None })?;
        self.calls.answer_question(id, answers)
    }

    /// The root's answer to an approval the broker delivered (EN-2a).
    async fn approve(&self, id: &str, verdict: ApprovalVerdict) -> Result<(), CallError> {
        self.calls
            .root_request(&Request::AnswerApproval { id, nonce: None })?;
        self.calls.answer_approval(id, verdict)
    }

    /// The handles of the root conversation `scope` (RO-4): the store's
    /// rows with the mail waiting for each.
    fn handles(&self, scope: &ConversationScope) -> Vec<HandleSnapshot> {
        self.calls.handle_rows(scope)
    }

    /// The root's next user turn is starting: its messages, for the turn
    /// to open with (delivery point b).
    fn take_run_messages(&self) -> Vec<String> {
        match self.calls.root_request(&Request::TakeRunMessages) {
            Ok(()) => self.calls.start_run(&Peer::Root),
            Err(_) => Vec::new(),
        }
    }

    /// The root stops one run of its swarm (TB-7).
    fn cancel(&self, node: &str) -> Result<(), CallError> {
        self.calls.root_request(&Request::Cancel { node })?;
        self.calls.cancel(node)
    }
}

#[cfg(test)]
#[path = "calls_tests.rs"]
mod tests;

#[cfg(all(test, unix))]
#[path = "swarm_forwarding_tests.rs"]
mod swarm_forwarding_tests;

#[cfg(all(test, unix))]
#[path = "task_tree_tests.rs"]
mod task_tree_tests;

#[cfg(all(test, unix))]
#[path = "root_approval_tests.rs"]
mod root_approval_tests;

#[cfg(all(test, unix))]
#[path = "question_relay_tests.rs"]
mod question_relay_tests;

#[cfg(all(test, unix))]
#[path = "gate_broker_tests.rs"]
mod gate_broker_tests;

#[cfg(test)]
#[path = "handle_broker_tests.rs"]
mod handle_broker_tests;
