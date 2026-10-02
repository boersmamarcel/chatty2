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
//! caller hanging up on the SSE stream does. That is how cancelling a
//! leader's task reaps its whole subtree: each hop's worker dies with the
//! call that started it, and its own calls die with its connection.
//!
//! A `send_message` call (tree messages, TM-1) never starts a run: the
//! broker checks the recipient against the caller's connection — the
//! sender's owner is the only recipient there is until live handles exist
//! (RC-3) — and queues the message on the recipient's [`PendingList`], or
//! refuses it. Either way the call's result is a [`MessageStatus`].
//!
//! A waiting message is delivered at exactly two points (TM-2): on the next
//! `invoke_agent` result its recipient receives, as that result's
//! `messages`, or at the start of the recipient's next run — the root's
//! next user turn ([`Transport::take_run_messages`]), a node's next task
//! ([`BrokerCalls::start_run`]). Never mid-run: nothing else reads a
//! pending list. It is delivered wrapped as untrusted data
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
//! ([`CallEvent::InputWithdrawn`] under the broker's id). A root call that
//! ends leaves every question still waiting on it unanswered, and a node no
//! call is running has nobody to ask. A call that ends withdraws whatever
//! its callee still asks.
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
//! the broker's id); a root call that ends denies whatever is still pending
//! under it. An approval with no root call to go to is denied.
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
//! Before an `invoke_agent` call spawns, submits or permits anything, the
//! broker checks it (PL-S2): a node's call against the specs'
//! [`CallPolicy`] (`delegates_to`, `exposed`, `callers`; DP-1's
//! `may_call`), then every call against its [`CallChain`] (DP-2). The chain
//! is the calling run's own, from the broker's task table, plus the callee:
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

use chatty_fabric::{
    AgentOrigin, Answer, ApprovalRequest, ApprovalVerdict, AskReply, AskRequest, Asker,
    CANCELLED_BY_USER, CallChain, CallError, CallEvent, CallPolicy, CallRequest, CallStream,
    ChildCall, ConversationScope, EdgeKind, EdgeLog, EdgeRow, FORWARD_INTERVAL, InvokeAgentOutcome,
    InvokeAgentParams, Message, MessageStatus, NodeId, NodeState, PendingList, ROOT_NAME, Refusal,
    RefusalReason, Remaining, SendMessageParams, SpawnContext, SwarmBatcher, SwarmItem, Transport,
    UsagePricer, deadline_grace,
};
use futures::StreamExt;
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};

use super::protocol::{CallStamp, DelegatedTask, TaskState};
use super::registry::{ParticipantRegistry, ROOT_SCOPE, TaskUpdate};
use super::spawn_context;
use super::virtual_agent::VirtualAgent;
use crate::handlers::a2a_participant;

/// Who a call is made as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Caller {
    /// The in-process root, through its [`DirectTransport`].
    Root,
    /// A node, by the name its connection was admitted under.
    Node(String),
}

impl Caller {
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
}

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

fn lock_approvals(
    approvals: &Approvals,
) -> std::sync::MutexGuard<'_, HashMap<String, PendingApproval>> {
    approvals.lock().unwrap_or_else(|e| e.into_inner())
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

    /// The root's answer; `Denied` when there is nobody to ask or the root
    /// call it went to has ended.
    pub(crate) async fn verdict(&mut self) -> ApprovalVerdict {
        match self.pending.as_mut() {
            Some((_, answer)) => answer.await.unwrap_or(ApprovalVerdict::Denied),
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
        let pending = lock_approvals(&self.approvals).remove(&id);
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
    caller: Caller,
    /// The chain the broker stamped on the call: its root call's id, and
    /// the specs from the root to the callee.
    chain: CallChain,
}

fn lock_routes(routes: &Routes) -> std::sync::MutexGuard<'_, HashMap<String, Route>> {
    routes.lock().unwrap_or_else(|e| e.into_inner())
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
            let mut routes = lock_routes(&self.routes);
            if !routes
                .get(&self.node)
                .is_some_and(|route| route.call == self.call)
            {
                return;
            }
            routes.remove(&self.node);
        }
        let asked: Vec<(String, PendingQuestion)> = {
            let mut questions = lock_questions(&self.questions);
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

fn lock_questions(
    questions: &Questions,
) -> std::sync::MutexGuard<'_, HashMap<String, PendingQuestion>> {
    questions.lock().unwrap_or_else(|e| e.into_inner())
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
    /// `Err` when nobody is left to ask, or the root call it reached ended
    /// without an answer.
    pub(crate) async fn answers(&mut self) -> Result<Vec<Answer>, CallError> {
        let unanswered = || CallError::Failed("the question went unanswered".to_string());
        loop {
            let (reply, replied) = oneshot::channel();
            let Some(route) = lock_routes(&self.routes).get(&self.from).cloned() else {
                warn!(participant = %self.from, question = %self.id, "A question with nobody to ask");
                return Err(nobody_above());
            };
            // The root call this hop's call is nested under.
            let root_task_id = route.chain.root_task_id;
            let hop = match route.caller {
                Caller::Root => {
                    let Some(root) = lock_swarm(&self.swarm).get(&root_task_id).cloned() else {
                        warn!(question = %self.id, "A question with no root call to ask");
                        return Err(unanswered());
                    };
                    Hop::Root(root)
                }
                Caller::Node(caller) => Hop::Node(caller),
            };
            // In the table before it is sent, so an answer — or a root call
            // that ends — in between finds it.
            lock_questions(&self.questions).insert(
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
                lock_questions(&self.questions).remove(&self.id);
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
                Err(_) => return Err(unanswered()),
            }
        }
    }
}

impl Drop for RaisedQuestion {
    fn drop(&mut self) {
        // Answered questions are already out of the table.
        let Some(pending) = lock_questions(&self.questions).remove(&self.id) else {
            return;
        };
        debug!(question = %self.id, "A question was withdrawn before it was answered");
        withdraw(&self.registry, &self.id, pending.at);
    }
}

/// The running calls [`BrokerCalls::cancel`] can stop.
type Stops = Arc<Mutex<Vec<Stoppable>>>;

/// One running call: the node its callee runs as, the name its caller
/// addressed, and the line that stops it.
struct Stoppable {
    id: u64,
    node: String,
    agent: String,
    by_root: bool,
    stop: oneshot::Sender<()>,
}

/// A running call's entry in [`Stops`]; leaves it when the call ends.
struct StopGuard {
    stops: Stops,
    id: u64,
}

impl Drop for StopGuard {
    fn drop(&mut self) {
        lock_stops(&self.stops).retain(|entry| entry.id != self.id);
    }
}

fn lock_stops(stops: &Stops) -> std::sync::MutexGuard<'_, Vec<Stoppable>> {
    stops.lock().unwrap_or_else(|e| e.into_inner())
}

/// Numbers [`Stoppable`] entries, unique in this process.
static NEXT_STOPPABLE: AtomicU64 = AtomicU64::new(0);

/// Put the call running `node` for `caller`, addressed as `agent`, on
/// `stops`: the receiver fires when the user stops it, and the guard takes
/// it off again.
fn stoppable(
    stops: &Stops,
    caller: &Caller,
    agent: &str,
    node: &str,
) -> (oneshot::Receiver<()>, StopGuard) {
    let id = NEXT_STOPPABLE.fetch_add(1, Ordering::Relaxed);
    let (stop, stopped) = oneshot::channel();
    lock_stops(stops).push(Stoppable {
        id,
        node: node.to_string(),
        agent: agent.to_string(),
        by_root: *caller == Caller::Root,
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
/// denies every approval still waiting on it and leaves every question
/// waiting on it unanswered.
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
        lock_swarm(swarm).insert(root_task_id.to_string(), tx);
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
        lock_swarm(&self.swarm).remove(&self.root_task_id);
        let mut approvals = lock_approvals(&self.approvals);
        let orphaned: Vec<String> = approvals
            .iter()
            .filter(|(_, pending)| pending.root_task_id == self.root_task_id)
            .map(|(id, _)| id.clone())
            .collect();
        for id in orphaned {
            if let Some(pending) = approvals.remove(&id) {
                let _ = pending.answer.send(ApprovalVerdict::Denied);
            }
        }
        drop(approvals);
        // Dropping a question's reply leaves it unanswered.
        lock_questions(&self.questions).retain(|_, pending| {
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

fn lock_swarm(
    swarm: &Swarm,
) -> std::sync::MutexGuard<'_, HashMap<String, mpsc::UnboundedSender<ToRoot>>> {
    swarm.lock().unwrap_or_else(|e| e.into_inner())
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
        }
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
        let Some(chain) = lock_routes(&self.routes)
            .get(node)
            .map(|route| route.chain.clone())
        else {
            warn!(participant = %node, "A question from a node no call is running");
            return Err(nobody_above());
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
        let mut questions = lock_questions(&self.questions);
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
        let mut questions = lock_questions(&self.questions);
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
        let Some(chain) = self.registry.run_chain(node) else {
            warn!(participant = %node, "Denying an approval from a node serving no run");
            return RaisedApproval::denied(&self.approvals);
        };
        let Some(root) = lock_swarm(&self.swarm).get(&chain.root_task_id).cloned() else {
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
        lock_approvals(&self.approvals).insert(
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
            lock_approvals(&self.approvals).remove(&id);
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
        let pending = lock_approvals(&self.approvals)
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
        let mut stops = lock_stops(&self.stops);
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
            let _ = entry.stop.send(());
        }
        Ok(())
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

    /// Price each callee's reported usage on its task row with `pricer`.
    pub fn with_pricer(mut self, pricer: Option<Arc<dyn UsagePricer>>) -> Self {
        self.pricer = pricer;
        self
    }

    /// Run `request` as `caller`. The stream is the call: progress, then one
    /// result or error. Dropping it cancels whatever the call started.
    pub fn call(&self, caller: Caller, request: CallRequest) -> CallStream {
        match request {
            CallRequest::InvokeAgent(params) => {
                // Checked first: a refused call spawns nothing and does not
                // touch the caller's permit either (PL-S2).
                let (edge, stamp) = match self.admit(&caller, &params) {
                    Ok(admitted) => admitted,
                    Err(refused) => return refused,
                };
                // Released now, before the callee queues for a slot that
                // may be this very one.
                let child = match &caller {
                    Caller::Node(name) => self.registry.node_permit(name).map(|p| p.child_call()),
                    Caller::Root => None,
                };
                let call = self.invoke(caller, params, edge, stamp);
                match child {
                    Some(child) => gated(child, call),
                    None => call,
                }
            }
            CallRequest::ListAgents => {
                futures::stream::iter([Ok(CallEvent::Result(self.directory()))]).boxed()
            }
            CallRequest::SendMessage(params) => {
                let status = self.send_message(&caller, params);
                futures::stream::iter([Ok(CallEvent::Result(json!(status)))]).boxed()
            }
        }
    }

    /// Queue `params` for its recipient as `caller`, or refuse it, and log
    /// one message row either way.
    fn send_message(&self, caller: &Caller, params: SendMessageParams) -> MessageStatus {
        let bytes = params.text.len() as u64;
        let to = params.to.clone();
        let status = self.accept_message(caller, params);
        let outcome = match &status {
            MessageStatus::Pending { .. } => "pending".to_string(),
            MessageStatus::Refused { reason } => format!("refused: {reason}"),
        };
        debug!(from = %caller.name(), %to, %outcome, "send_message");
        EdgeGuard {
            log: self.edges.clone(),
            from: caller.name().to_string(),
            to,
            chain: vec![caller.name().to_string()],
            bytes,
            outcome: None,
            usd: None,
        }
        .write(EdgeKind::Message, outcome);
        status
    }

    /// The recipient check and the pending list's bounds. The sender is who
    /// its connection says, and its owner is who the directory says: the
    /// message names only the recipient, and a name that is not the
    /// sender's owner — a sibling, the sender itself, a name nobody has, a
    /// node of another conversation — is not on the tree.
    fn accept_message(&self, caller: &Caller, params: SendMessageParams) -> MessageStatus {
        let refused = |reason| MessageStatus::Refused { reason };
        // The root has no owner, and its handles come with resumable
        // conversations (RC-3).
        let Caller::Node(name) = caller else {
            return refused(RefusalReason::NotOnTree);
        };
        let Some((sender, owner)) = self.registry.node_and_owner(name) else {
            return refused(RefusalReason::NotOnTree);
        };
        let (recipient, owner_name, ended) = match &owner {
            None => (Recipient::Root, ROOT_NAME, false),
            Some(owner) => (
                Recipient::Node(owner.id()),
                owner.name().as_str(),
                owner.state() == NodeState::Ended,
            ),
        };
        if params.to != owner_name {
            return refused(RefusalReason::NotOnTree);
        }
        if ended {
            return refused(RefusalReason::RecipientEnded);
        }

        let id = format!(
            "msg-{}",
            self.next_message.fetch_add(1, Ordering::Relaxed) + 1
        );
        let message = Message {
            id: id.clone(),
            from: sender.id(),
            from_name: sender.name().clone(),
            text: params.text,
        };
        let mut pending = lock(&self.pending);
        match pending.entry(recipient).or_default().push(message) {
            Ok(()) => MessageStatus::Pending { id },
            Err(reason) => refused(reason),
        }
    }

    /// Whose pending list `caller` reads: the root's, or its node's. `None`
    /// for a name no node was admitted under, which has nothing waiting.
    fn inbox(&self, caller: &Caller) -> Option<Recipient> {
        match caller {
            Caller::Root => Some(Recipient::Root),
            Caller::Node(name) => self
                .registry
                .node_and_owner(name)
                .map(|(node, _)| Recipient::Node(node.id())),
        }
    }

    /// `caller` is starting a new run — the root's next user turn, a node's
    /// next task: take what is waiting for it, wrapped and oldest first,
    /// and give each sender its allowance back (delivery point b).
    pub fn start_run(&self, caller: &Caller) -> Vec<String> {
        let Some(inbox) = self.inbox(caller) else {
            return Vec::new();
        };
        let mut pending = lock(&self.pending);
        let Some(list) = pending.get_mut(&inbox) else {
            return Vec::new();
        };
        list.start_run();
        deliver(list)
    }

    /// The node `id`, admitted as `name`, has ended: what was waiting for it
    /// is dropped, one `message` row per message with outcome `dropped`.
    /// Later messages to it are `recipient_ended`.
    pub(crate) fn recipient_ended(&self, id: NodeId, name: &str) {
        let dropped = lock(&self.pending)
            .remove(&Recipient::Node(id))
            .map(|mut list| list.take_all())
            .unwrap_or_default();
        for message in dropped {
            debug!(from = %message.from_name, to = %name, id = %message.id, "Dropped a message with its recipient");
            EdgeGuard {
                log: self.edges.clone(),
                from: message.from_name.as_str().to_string(),
                to: name.to_string(),
                chain: vec![message.from_name.as_str().to_string()],
                bytes: message.bytes() as u64,
                outcome: None,
                usd: None,
            }
            .write(EdgeKind::Message, "dropped".to_string());
        }
    }

    /// Every agent a call can address, as the aggregated agent card lists
    /// them: connected participants, then virtual agents, each with its
    /// origin (ADR-0011 C5).
    fn directory(&self) -> Value {
        let participants = self
            .registry
            .agents()
            .into_iter()
            .map(|agent| with_origin(a2a_participant::card_to_json(&agent.card), agent.origin));
        let runners = self.runners.values().map(|runner| {
            with_origin(
                a2a_participant::card_to_json(&runner.agent_card()),
                AgentOrigin::Local,
            )
        });
        Value::Array(participants.chain(runners).collect())
    }

    /// Who may call whom, and how far, before anything is spawned,
    /// submitted or permitted (PL-S2): the call's edge-log row and the stamp
    /// for the run it starts, or its refusal, already logged. An agent
    /// nobody serves is unknown, which [`invoke`](Self::invoke) says.
    fn admit(
        &self,
        caller: &Caller,
        params: &InvokeAgentParams,
    ) -> Result<(EdgeGuard, Option<CallStamp>), CallStream> {
        let caller_chain = self.caller_chain(caller);
        let mut edge = EdgeGuard {
            log: self.edges.clone(),
            from: caller.name().to_string(),
            to: params.agent.clone(),
            chain: caller_chain.chain.clone(),
            bytes: params.prompt.len() as u64,
            outcome: None,
            usd: None,
        };
        if !self.runners.contains_key(&params.agent) && !self.registry.is_registered(&params.agent)
        {
            return Ok((edge, None));
        }
        match self.check(caller, caller_chain, &params.agent, &params.remaining) {
            Ok(stamp) => Ok((edge, Some(stamp))),
            Err(refusal) => {
                warn!(caller = %caller.name(), agent = %params.agent, %refusal, "Refused a call");
                edge.refused(&refusal.to_string());
                Err(futures::stream::iter([Err(CallError::Delegation(refusal))]).boxed())
            }
        }
    }

    fn invoke(
        &self,
        caller: Caller,
        params: InvokeAgentParams,
        mut edge: EdgeGuard,
        stamp: Option<CallStamp>,
    ) -> CallStream {
        let registry = self.registry.clone();
        let runner = self.runners.get(&params.agent).cloned();
        // A worker the call starts gets its context from the caller's own
        // (BI-5); a context that reaches outside it ends the call here.
        let spawn = match runner.as_ref() {
            Some(runner) if !registry.is_registered(&params.agent) => {
                self.spawn_context(&caller, runner.as_ref(), params.spawn_context)
            }
            _ => Ok(None),
        };
        // When the broker stops a callee that outran its deadline (DP-3).
        let cut = stamp
            .as_ref()
            .and_then(|stamp| stamp.chain.deadline)
            .map(|deadline| {
                let left = deadline
                    .duration_since(std::time::SystemTime::now())
                    .unwrap_or_default();
                tokio::time::Instant::now() + left + deadline_grace(left)
            });
        let pricer = self.pricer.clone();
        let stops = self.stops.clone();
        let routes = self.routes.clone();
        let questions = self.questions.clone();
        // A root call listens for the runs nested under it; a run a node's
        // call starts reports to its root call, if that is listening
        // (TB-1). Both are keyed by the chain the broker stamped.
        let chain = stamp.as_ref().map(|stamp| stamp.chain.clone());
        let route_chain = chain.clone();
        let mut listening = match (&caller, &chain) {
            (Caller::Root, Some(chain)) => Some(Listening::open(
                &self.swarm,
                &self.approvals,
                &self.questions,
                &chain.root_task_id,
            )),
            _ => None,
        };
        let reports_to = match (&caller, chain) {
            (Caller::Node(_), Some(chain)) => lock_swarm(&self.swarm)
                .get(&chain.root_task_id)
                .cloned()
                .map(|to| (to, chain)),
            _ => None,
        };
        let task = DelegatedTask::new(params.prompt)
            .with_call(stamp)
            .with_swarm_events(reports_to.is_some());
        let agent = params.agent;
        // The caller's messages ride on this call's result (delivery point
        // a), taken when the result is made.
        let inbox = self
            .inbox(&caller)
            .map(|inbox| (inbox, self.pending.clone()));
        let messages = move || match &inbox {
            Some((inbox, pending)) => lock(pending)
                .get_mut(inbox)
                .map(deliver)
                .unwrap_or_default(),
            None => Vec::new(),
        };

        async_stream::stream! {
            let task = match spawn {
                Ok(context) => task.with_spawn_context(context),
                Err(error) => {
                    warn!(caller = %caller.name(), agent = %agent, %error, "Refused a spawn context");
                    edge.refused(&error.to_string());
                    yield Err(error);
                    return;
                }
            };
            let running = if registry.is_registered(&agent) {
                a2a_participant::submit(&registry, &agent, task).await
                    .ok_or_else(|| format!("participant '{agent}' is no longer connected"))
            } else if let Some(runner) = runner {
                info!(caller = %caller.name(), agent = %agent, "Starting a worker for a call");
                a2a_participant::spawn(runner.as_ref(), task).await
            } else {
                edge.refused("unknown agent");
                yield Err(CallError::UnknownAgent(agent));
                return;
            };
            let mut running = match running {
                Ok(running) => running,
                Err(reason) => {
                    warn!(agent = %agent, %reason, "Could not start a worker for a call");
                    edge.end(TaskState::Failed);
                    yield Ok(CallEvent::Result(outcome(TaskState::Failed, String::new(), Some(reason), None, messages())));
                    return;
                }
            };
            edge.to = running.participant().to_string();
            // Where the callee's questions go while this call runs it
            // (EN-2b): before anything is yielded, so a question the worker
            // asks at once finds it.
            let _route = route_chain.map(|chain| {
                let call = NEXT_STOPPABLE.fetch_add(1, Ordering::Relaxed);
                lock_routes(&routes).insert(
                    running.participant().to_string(),
                    Route { call, caller: caller.clone(), chain },
                );
                RouteGuard {
                    routes: routes.clone(),
                    questions: questions.clone(),
                    registry: registry.clone(),
                    node: running.participant().to_string(),
                    call,
                }
            });
            // As soon as admitted, before anything else: a caller that
            // only knew this callee by its spec can now name this one
            // call precisely (AGE-762), which two parallel calls to the
            // same spec need to be stoppable one at a time.
            yield Ok(CallEvent::Progress(json!({ "Admitted": running.participant() })));
            let (mut stopped, _stoppable) = stoppable(&stops, &caller, &agent, running.participant());
            let mut stopped_by_user = false;
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
                        end = Some((
                            TaskState::Failed,
                            Some("deadline: the call ran past its deadline and was stopped".to_string()),
                            None,
                        ));
                        break;
                    }
                    Ok(()) = &mut stopped => {
                        stopped_by_user = true;
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
                            yield Ok(CallEvent::Progress(json!({ "Text": text })));
                        }
                    }
                    TaskUpdate::Event(item) => {
                        if item.is_workers_to_report() {
                            report(item);
                        }
                    }
                    TaskUpdate::Status { state, message, metadata } => {
                        if state.is_terminal() {
                            if let Some(usage) = metadata.as_ref().and_then(|m| m.get("usage")) {
                                report(SwarmItem::Usage { usage: usage.clone() });
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
                                yield Ok(CallEvent::Progress(json!({ "Text": evidence.text })));
                            }
                            end = Some((
                                state,
                                message,
                                a2a_participant::with_evidence(metadata, evidence.as_ref()),
                            ));
                            break;
                        }
                        if state == TaskState::Working
                            && let Some(step) = message
                        {
                            yield Ok(CallEvent::Progress(json!({ "Step": step })));
                        }
                    }
                }
            }

            if stopped_by_user {
                // The subtree goes now, not when the caller lets go of the
                // stream: the worker is reaped, its connection closes, and
                // every call it made is dropped with it.
                drop(running);
                report(SwarmItem::Ended { state: TaskState::Canceled.to_string() });
                edge.write(EdgeKind::Task, "cancelled".to_string());
                yield Ok(CallEvent::Result(json!(InvokeAgentOutcome {
                    success: false,
                    response,
                    error: Some(CANCELLED_BY_USER.to_string()),
                    metadata: None,
                    messages: messages(),
                    cancelled_by_user: true,
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
                .zip(metadata.as_ref())
                .and_then(|(pricer, metadata)| pricer.usd(metadata));
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
            edge.end(state);
            yield Ok(CallEvent::Result(outcome(state, response, message, metadata, messages())));
            // `running` is dropped here, which reaps a spawned worker.
        }
        .boxed()
    }

    /// The chain `caller` calls from: the root's own, fresh, or the chain
    /// of the run the node serves in the broker's task table. A node no
    /// broker call started (one an A2A request over HTTP spawned) is the
    /// root's callee.
    fn caller_chain(&self, caller: &Caller) -> CallChain {
        let root = || CallChain::root(uuid::Uuid::new_v4().to_string());
        match caller {
            Caller::Root => root(),
            Caller::Node(name) => self.registry.run_chain(name).unwrap_or_else(|| {
                let spec = self.caller_spec(name);
                root().extend(&spec).unwrap_or_else(|_| root())
            }),
        }
    }

    /// The spec a node was admitted as; its name when it was never admitted.
    fn caller_spec(&self, name: &str) -> String {
        self.registry
            .node_spec(name)
            .unwrap_or_else(|| name.to_string())
    }

    /// Whether `caller`, at `chain`, may call `agent`: the specs first (a
    /// node's as the spec it was admitted as, the root's as the spec the
    /// policy says it runs as), then the chain's cycle and depth, then its budget
    /// narrowed by `caller_left`, what the caller says it has left (DP-3).
    /// The run the call starts is stamped with the chain it runs under,
    /// budget included.
    fn check(
        &self,
        caller: &Caller,
        chain: CallChain,
        agent: &str,
        caller_left: &Remaining,
    ) -> Result<CallStamp, Refusal> {
        // A registered participant is addressed by its node name; the chain
        // and the policy speak in specs.
        let callee = self
            .registry
            .node_spec(agent)
            .unwrap_or_else(|| agent.to_string());
        let policy = self.policy.get();
        let caller = match caller {
            Caller::Root => {
                policy.root_may_call(&callee)?;
                None
            }
            Caller::Node(name) => {
                policy.may_call(&self.caller_spec(name), &callee)?;
                Some(name.clone())
            }
        };
        let own = policy.budget(&callee);
        let chain =
            chain
                .extend(&callee)?
                .budget(caller_left, &own, std::time::SystemTime::now())?;
        Ok(CallStamp { caller, chain })
    }

    /// The context a worker spawned for `caller` as `target` starts from:
    /// derived from the caller's own when the call brings none, clamped to
    /// it when it does (invariant 6). A node no runner recorded a context
    /// for is the root's.
    ///
    /// A virtual agent outside the caller's roster is refused: a sub-leader
    /// reaches only what it was given.
    fn spawn_context(
        &self,
        caller: &Caller,
        target: &dyn VirtualAgent,
        requested: Option<SpawnContext>,
    ) -> Result<Option<SpawnContext>, CallError> {
        let own = match caller {
            Caller::Node(name) => self.registry.node_context(name),
            Caller::Root => None,
        }
        .unwrap_or_else(|| spawn_context::root(&self.runners, target));
        if !own.roster.iter().any(|name| name == target.agent_name()) {
            return Err(CallError::Refused(format!(
                "'{}' is not on {}'s roster",
                target.agent_name(),
                caller.name()
            )));
        }
        match requested {
            None => Ok(Some(spawn_context::derive(&own, target))),
            Some(requested) => spawn_context::clamp(requested, &own, target).map(Some),
        }
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

/// The result of an `agent.invoke` request whose task ended in `state`,
/// carrying the caller's waiting `messages`. Only a failure is a failure: a
/// task that was cancelled from its own side reads as it does to an A2A
/// caller.
fn outcome(
    state: TaskState,
    response: String,
    message: Option<String>,
    metadata: Option<Value>,
    messages: Vec<String>,
) -> Value {
    let success = state != TaskState::Failed;
    json!(InvokeAgentOutcome {
        success,
        response,
        error: if success { None } else { message },
        metadata,
        messages,
        cancelled_by_user: false,
    })
}

/// Take everything waiting on `list`, as its recipient reads it.
fn deliver(list: &mut PendingList) -> Vec<String> {
    list.take_all().iter().map(Message::wrapped).collect()
}

/// The pending lists, recovered from a poisoned lock: each list is plain
/// owned data with no invariant a panic could leave half-kept.
fn lock(
    pending: &Mutex<HashMap<Recipient, PendingList>>,
) -> std::sync::MutexGuard<'_, HashMap<Recipient, PendingList>> {
    pending.lock().unwrap_or_else(|e| e.into_inner())
}

/// Tag one agent card with its origin, as the aggregated card does.
fn with_origin(mut card: Value, origin: AgentOrigin) -> Value {
    if let Some(object) = card.as_object_mut() {
        object.insert("origin".to_string(), json!(origin.as_str()));
    }
    card
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
}

impl DirectTransport {
    pub fn new(calls: Arc<BrokerCalls>) -> Self {
        Self { calls }
    }
}

#[async_trait::async_trait]
impl Transport for DirectTransport {
    async fn call(&self, req: CallRequest) -> Result<CallStream, CallError> {
        Ok(self.calls.call(Caller::Root, req))
    }

    /// The root's answers to a question the broker delivered (EN-2b).
    async fn answer(&self, id: &str, answers: Vec<Answer>) -> Result<(), CallError> {
        self.calls.answer_question(id, answers)
    }

    /// The root's answer to an approval the broker delivered (EN-2a).
    async fn approve(&self, id: &str, verdict: ApprovalVerdict) -> Result<(), CallError> {
        self.calls.answer_approval(id, verdict)
    }

    /// The root's next user turn is starting: its messages, for the turn
    /// to open with (delivery point b).
    fn take_run_messages(&self) -> Vec<String> {
        self.calls.start_run(&Caller::Root)
    }

    /// The root stops one run of its swarm (TB-7).
    fn cancel(&self, node: &str) -> Result<(), CallError> {
        self.calls.cancel(node)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chatty_fabric::{PENDING_LIST_BYTES, SENDER_ALLOWANCE_BYTES};

    fn broker(edges: Option<Arc<Mutex<EdgeLog>>>) -> (Arc<BrokerCalls>, ParticipantRegistry) {
        let registry = ParticipantRegistry::new();
        let calls = Arc::new(BrokerCalls::new(
            registry.clone(),
            Arc::new(BTreeMap::new()),
            edges,
        ));
        registry.install_calls(&calls);
        (calls, registry)
    }

    /// The edge log's rows as `(kind, from, to, bytes, outcome)`.
    fn rows(path: &std::path::Path) -> Vec<(EdgeKind, String, String, u64, String)> {
        std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<EdgeRow>(line).unwrap())
            .map(|row| (row.kind, row.from, row.to, row.bytes, row.outcome))
            .collect()
    }

    async fn send(calls: &BrokerCalls, caller: Caller, to: &str, text: &str) -> MessageStatus {
        let mut stream = calls.call(
            caller,
            CallRequest::SendMessage(SendMessageParams {
                to: to.to_string(),
                text: text.to_string(),
            }),
        );
        let Some(Ok(CallEvent::Result(value))) = stream.next().await else {
            panic!("a send_message call answers with one result");
        };
        assert!(stream.next().await.is_none());
        serde_json::from_value(value).expect("a MessageStatus")
    }

    fn node(name: &str) -> Caller {
        Caller::Node(name.to_string())
    }

    fn refused(reason: RefusalReason) -> MessageStatus {
        MessageStatus::Refused { reason }
    }

    /// The owner is the only recipient: the root for a node the root owns,
    /// the owning node for one a node owns; siblings, self, a grandparent,
    /// unknown names, an unknown sender and the root itself are refused.
    #[tokio::test]
    async fn only_the_owner_is_on_the_tree() {
        let (calls, registry) = broker(None);
        let lead = registry.admit_under("lead", None);
        let coder = registry.admit_under("coder", Some(&lead));
        let other = registry.admit_under("coder", Some(&lead));

        assert_eq!(
            send(&calls, node(&lead), ROOT_NAME, "up").await,
            MessageStatus::Pending { id: "msg-1".into() }
        );
        assert_eq!(
            send(&calls, node(&coder), &lead, "up").await,
            MessageStatus::Pending { id: "msg-2".into() }
        );
        for (from, to) in [
            (coder.as_str(), other.as_str()),
            (coder.as_str(), coder.as_str()),
            (coder.as_str(), ROOT_NAME),
            (coder.as_str(), "nobody-0"),
            (lead.as_str(), coder.as_str()),
            ("never-admitted-0", ROOT_NAME),
        ] {
            assert_eq!(
                send(&calls, node(from), to, "x").await,
                refused(RefusalReason::NotOnTree),
                "{from} -> {to}"
            );
        }
        assert_eq!(
            send(&calls, Caller::Root, &lead, "x").await,
            refused(RefusalReason::NotOnTree),
            "the root's handles come with RC-3"
        );
    }

    #[tokio::test]
    async fn a_message_to_an_ended_owner_is_refused() {
        let (calls, registry) = broker(None);
        let lead = registry.admit_under("lead", None);
        let coder = registry.admit_under("coder", Some(&lead));
        registry.end_node(&lead);
        assert_eq!(
            send(&calls, node(&coder), &lead, "too late").await,
            refused(RefusalReason::RecipientEnded)
        );
    }

    /// The pending list's bounds hold through the broker: per sender per
    /// run, then per recipient, each recipient with a list of its own.
    #[tokio::test]
    async fn the_broker_enforces_the_pending_list_bounds() {
        let (calls, registry) = broker(None);
        let lead = registry.admit_under("lead", None);
        let coders: Vec<String> = (0..9)
            .map(|_| registry.admit_under("coder", Some(&lead)))
            .collect();
        let allowance = "x".repeat(SENDER_ALLOWANCE_BYTES);

        assert!(matches!(
            send(&calls, node(&coders[0]), &lead, &allowance).await,
            MessageStatus::Pending { .. }
        ));
        assert_eq!(
            send(&calls, node(&coders[0]), &lead, "one byte more").await,
            refused(RefusalReason::OverAllowance)
        );
        for coder in &coders[1..PENDING_LIST_BYTES / SENDER_ALLOWANCE_BYTES] {
            assert!(matches!(
                send(&calls, node(coder), &lead, &allowance).await,
                MessageStatus::Pending { .. }
            ));
        }
        assert_eq!(
            send(&calls, node(&coders[8]), &lead, "x").await,
            refused(RefusalReason::OverAllowance),
            "the lead's list is full"
        );
        assert!(
            matches!(
                send(&calls, node(&lead), ROOT_NAME, &allowance).await,
                MessageStatus::Pending { .. }
            ),
            "the root's list is another list"
        );
    }

    /// Rule 5: a recipient that ends drops what was waiting for it, one
    /// `dropped` row per message; nothing delivers them afterwards, and a
    /// later message to it is `recipient_ended`. Another recipient's list
    /// is untouched.
    #[tokio::test]
    async fn messages_dropped_with_recipient() {
        let data = tempfile::tempdir().unwrap();
        let log = EdgeLog::open(data.path()).unwrap();
        let path = log.path();
        let (calls, registry) = broker(Some(Arc::new(Mutex::new(log))));
        let lead = registry.admit_under("lead", None);
        let coder = registry.admit_under("coder", Some(&lead));
        let other = registry.admit_under("coder", Some(&lead));

        send(&calls, node(&coder), &lead, "first").await;
        send(&calls, node(&other), &lead, "second one").await;
        send(&calls, node(&lead), ROOT_NAME, "for the root").await;
        registry.end_node(&lead);

        assert!(
            calls.start_run(&node(&lead)).is_empty(),
            "nothing is left to deliver"
        );
        assert_eq!(
            send(&calls, node(&coder), &lead, "too late").await,
            refused(RefusalReason::RecipientEnded)
        );
        let dropped: Vec<_> = rows(&path)
            .into_iter()
            .filter(|row| row.4 == "dropped")
            .collect();
        assert_eq!(
            dropped,
            [
                (
                    EdgeKind::Message,
                    coder.clone(),
                    lead.clone(),
                    5,
                    "dropped".into()
                ),
                (
                    EdgeKind::Message,
                    other.clone(),
                    lead.clone(),
                    10,
                    "dropped".into()
                ),
            ]
        );
        assert_eq!(
            calls.start_run(&Caller::Root),
            [format!(
                "<message from=\"{lead}\" untrusted=\"true\">for the root</message>"
            )],
            "the root's own list is not the ended node's"
        );
    }

    /// Delivery point b for the root: its next run takes what is waiting,
    /// wrapped and oldest first, exactly once, and gives each sender its
    /// allowance back.
    #[tokio::test]
    async fn the_roots_next_run_takes_its_messages() {
        let (calls, registry) = broker(None);
        let lead = registry.admit_under("lead", None);
        let root = DirectTransport::new(calls.clone());
        let allowance = "x".repeat(SENDER_ALLOWANCE_BYTES - 3);

        send(&calls, node(&lead), ROOT_NAME, "one").await;
        send(&calls, node(&lead), ROOT_NAME, &allowance).await;
        assert_eq!(
            send(&calls, node(&lead), ROOT_NAME, "x").await,
            refused(RefusalReason::OverAllowance)
        );
        let wrap =
            |text: &str| format!("<message from=\"{lead}\" untrusted=\"true\">{text}</message>");
        assert_eq!(root.take_run_messages(), [wrap("one"), wrap(&allowance)]);
        assert!(root.take_run_messages().is_empty(), "delivered once");
        assert!(
            matches!(
                send(&calls, node(&lead), ROOT_NAME, "x").await,
                MessageStatus::Pending { .. }
            ),
            "a new run gives the sender its allowance back"
        );
    }

    /// Delivery point b for a node: its next task opens with what is
    /// waiting for it.
    #[tokio::test]
    async fn a_nodes_next_task_opens_with_its_messages() {
        use super::super::protocol::{BrokerFrame, ParticipantCard};

        let (calls, registry) = broker(None);
        let admitted = registry.admit("lead", AgentOrigin::Local, None).unwrap();
        let (tx, mut outbound) = tokio::sync::mpsc::channel(8);
        let lead = registry.register(admitted, ParticipantCard::default(), tx);
        let coder = registry.admit_under("coder", Some(&lead));
        send(&calls, node(&coder), &lead, "<b>tests pass</b>").await;

        registry
            .submit_task(&lead, DelegatedTask::new("next task"))
            .await
            .expect("the lead is registered");
        let Some(BrokerFrame::Task { text, .. }) = outbound.recv().await else {
            panic!("a task frame");
        };
        assert_eq!(
            text,
            format!(
                "<message from=\"{coder}\" untrusted=\"true\">&lt;b&gt;tests pass&lt;/b&gt;</message>\n\nnext task"
            )
        );
        registry
            .submit_task(&lead, DelegatedTask::new("and another"))
            .await
            .unwrap();
        let Some(BrokerFrame::Task { text, .. }) = outbound.recv().await else {
            panic!("a task frame");
        };
        assert_eq!(text, "and another", "delivered once");
    }

    /// One message row per call, pending or refused, with the body's size.
    #[tokio::test]
    async fn every_message_writes_one_message_row() {
        let data = tempfile::tempdir().unwrap();
        let log = EdgeLog::open(data.path()).unwrap();
        let path = log.path();
        let (calls, registry) = broker(Some(Arc::new(Mutex::new(log))));
        let lead = registry.admit_under("lead", None);

        send(&calls, node(&lead), ROOT_NAME, "hello").await;
        send(&calls, node(&lead), "nobody-0", "hi").await;

        let rows: Vec<EdgeRow> = std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let rows: Vec<_> = rows
            .iter()
            .map(|row| {
                (
                    row.kind,
                    row.from.as_str(),
                    row.to.as_str(),
                    row.bytes,
                    row.outcome.as_str(),
                )
            })
            .collect();
        assert_eq!(
            rows,
            [
                (EdgeKind::Message, lead.as_str(), ROOT_NAME, 5, "pending"),
                (
                    EdgeKind::Message,
                    lead.as_str(),
                    "nobody-0",
                    2,
                    "refused: not_on_tree"
                ),
            ]
        );
    }

    /// A virtual agent that counts the workers it was asked for and starts
    /// none.
    struct Counting {
        name: String,
        registry: ParticipantRegistry,
        spawned: Arc<AtomicU64>,
    }

    impl VirtualAgent for Counting {
        fn agent_name(&self) -> &str {
            &self.name
        }

        fn agent_card(&self) -> super::super::protocol::ParticipantCard {
            super::super::protocol::ParticipantCard {
                name: self.name.clone(),
                display_name: None,
                description: String::new(),
                version: String::new(),
                skills: Vec::new(),
            }
        }

        fn registry(&self) -> &ParticipantRegistry {
            &self.registry
        }

        fn run_task(&self, _task: DelegatedTask) -> super::super::virtual_agent::WorkerFuture<'_> {
            self.spawned.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Err(anyhow::anyhow!("nothing is spawned here")) })
        }
    }

    /// A virtual agent whose one worker takes `delay` to answer: what a
    /// fake model that `Delay`s looks like from the broker. It records the
    /// budget its task frame would carry and when its worker was reaped.
    struct Slow {
        registry: ParticipantRegistry,
        delay: std::time::Duration,
        budget: Arc<Mutex<Option<Remaining>>>,
        reaped: Arc<Mutex<Option<tokio::time::Instant>>>,
    }

    struct SlowWorker {
        reaped: Arc<Mutex<Option<tokio::time::Instant>>>,
    }

    impl super::super::virtual_agent::WorkerHandle for SlowWorker {
        fn name(&self) -> &str {
            "kit-slow-0"
        }

        fn task_id(&self) -> Option<&str> {
            Some("task-slow")
        }

        fn finish(&mut self, _succeeded: bool, _metadata: Option<&Value>) {}
    }

    impl Drop for SlowWorker {
        fn drop(&mut self) {
            *self.reaped.lock().unwrap() = Some(tokio::time::Instant::now());
        }
    }

    impl VirtualAgent for Slow {
        fn agent_name(&self) -> &str {
            "kit-slow"
        }

        fn agent_card(&self) -> super::super::protocol::ParticipantCard {
            super::super::protocol::ParticipantCard {
                name: "kit-slow".to_string(),
                display_name: None,
                description: String::new(),
                version: String::new(),
                skills: Vec::new(),
            }
        }

        fn registry(&self) -> &ParticipantRegistry {
            &self.registry
        }

        fn run_task(&self, task: DelegatedTask) -> super::super::virtual_agent::WorkerFuture<'_> {
            *self.budget.lock().unwrap() = Some(task.frame_budget());
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            let delay = self.delay;
            tokio::spawn(async move {
                tokio::time::sleep(delay).await;
                let _ = tx.send(TaskUpdate::Status {
                    state: TaskState::Completed,
                    message: None,
                    metadata: None,
                });
            });
            let worker = SlowWorker {
                reaped: self.reaped.clone(),
            };
            Box::pin(async move {
                Ok((
                    Box::new(worker) as Box<dyn super::super::virtual_agent::WorkerHandle>,
                    rx,
                ))
            })
        }
    }

    /// Invariant 5 (DP-3): a callee started with 30 s left whose model
    /// takes 40 s is stopped by its deadline (plus the grace a headless run
    /// gives its own pass), and the caller gets a failed result, not a
    /// hang. On tokio's paused clock: no real second passes.
    #[tokio::test(start_paused = true)]
    async fn deadline_propagates() {
        let data = tempfile::tempdir().unwrap();
        let log = EdgeLog::open(data.path()).unwrap();
        let path = log.path();
        let registry = ParticipantRegistry::new();
        let budget = Arc::new(Mutex::new(None));
        let reaped = Arc::new(Mutex::new(None));
        let slow: Arc<dyn VirtualAgent> = Arc::new(Slow {
            registry: registry.clone(),
            delay: std::time::Duration::from_secs(40),
            budget: budget.clone(),
            reaped: reaped.clone(),
        });
        let calls = BrokerCalls::new(
            registry.clone(),
            Arc::new(BTreeMap::from([("kit-slow".to_string(), slow)])),
            Some(Arc::new(Mutex::new(log))),
        );

        let start = tokio::time::Instant::now();
        let events: Vec<_> = calls
            .call(
                Caller::Root,
                CallRequest::InvokeAgent(InvokeAgentParams {
                    agent: "kit-slow".to_string(),
                    prompt: "Take your time.".to_string(),
                    handle: None,
                    include_trace: false,
                    spawn_context: None,
                    remaining: Remaining {
                        seconds: Some(30),
                        ..Remaining::default()
                    },
                }),
            )
            .collect()
            .await;
        let ended = start.elapsed();

        let Some(Ok(CallEvent::Result(result))) = events.last() else {
            panic!("the call ends with a result: {events:?}");
        };
        let outcome: InvokeAgentOutcome = serde_json::from_value(result.clone()).unwrap();
        assert!(!outcome.success, "{outcome:?}");
        assert!(
            outcome
                .error
                .as_deref()
                .is_some_and(|e| e.starts_with("deadline:")),
            "{outcome:?}"
        );

        let seconds = budget.lock().unwrap().clone().unwrap().seconds;
        assert!(
            seconds.is_some_and(|s| (29..=30).contains(&s)),
            "the callee's task carries the 30 s its caller had left: {seconds:?}"
        );
        let grace = deadline_grace(std::time::Duration::from_secs(30));
        assert!(
            ended <= std::time::Duration::from_secs(30) + grace,
            "stopped by the deadline, after {ended:?}"
        );
        assert!(
            ended < std::time::Duration::from_secs(40),
            "not after the model answered"
        );
        let reaped = reaped.lock().unwrap().expect("the worker was reaped");
        assert!(
            reaped - start <= ended,
            "the worker was reaped with the call"
        );

        assert_eq!(
            rows(&path),
            [(
                EdgeKind::Task,
                ROOT_NAME.to_string(),
                "kit-slow-0".to_string(),
                "Take your time.".len() as u64,
                "failed".to_string()
            )]
        );
    }

    /// EN-0a (AGE-765): a broker built with no policy consults the named
    /// [`LocalPermissive`], and delegation behaves as it did when the check
    /// was skipped: the root and every node may call any spec, no spec
    /// brings a budget, and the chain still refuses a cycle or a call too
    /// deep.
    #[test]
    fn absent_policy_is_local_permissive() {
        let (calls, registry) = broker(None);
        assert!(
            matches!(calls.policy, Policy::LocalPermissive(_)),
            "no policy is LocalPermissive"
        );
        let given_none =
            BrokerCalls::new(registry, Arc::new(BTreeMap::new()), None).with_policy(None);
        assert!(
            matches!(given_none.policy, Policy::LocalPermissive(_)),
            "`with_policy(None)` is LocalPermissive too"
        );

        let left = Remaining::default();
        let root = CallChain::root("t-permissive");
        let stamp = calls
            .check(&Caller::Root, root.clone(), "anyone", &left)
            .expect("a plain root may call any spec");
        assert_eq!(stamp.caller, None);
        assert_eq!(stamp.chain.chain, ["root", "anyone"]);
        assert_eq!(stamp.chain.remaining, Remaining::default(), "no own budget");

        let at_one = root.extend("kit-1").unwrap();
        let stamp = calls
            .check(&node("kit-1-0"), at_one.clone(), "kit-2", &left)
            .expect("a node may call any spec");
        assert_eq!(stamp.caller.as_deref(), Some("kit-1-0"));
        assert_eq!(stamp.chain.chain, ["root", "kit-1", "kit-2"]);

        assert!(matches!(
            calls.check(&node("kit-1-0"), at_one, "kit-1", &left),
            Err(Refusal::Cycle { .. })
        ));
        let mut deepest = CallChain::root("t-deep");
        for spec in ["a", "b", "c", "d"] {
            deepest = deepest.extend(spec).unwrap();
        }
        assert_eq!(
            calls.check(&node("d-0"), deepest, "e", &left).err(),
            Some(Refusal::TooDeep { depth: 5, max: 4 })
        );
    }

    /// Invariant 4 (DP-2): a worker at depth 4 sends a call frame whose
    /// `params.metadata.chatty.call` claims depth 0 and an empty chain. The
    /// broker parses the frame as the wire gives it, reads the caller's
    /// chain from its own task table, and refuses the call at its real
    /// depth — and a cycle as a cycle — with nothing spawned.
    #[tokio::test]
    async fn forged_chain_is_ignored() {
        let data = tempfile::tempdir().unwrap();
        let log = EdgeLog::open(data.path()).unwrap();
        let path = log.path();
        let registry = ParticipantRegistry::new();
        let spawned = Arc::new(AtomicU64::new(0));
        let runners: BTreeMap<String, Arc<dyn VirtualAgent>> = ["kit-2", "kit-5"]
            .into_iter()
            .map(|name| {
                let runner: Arc<dyn VirtualAgent> = Arc::new(Counting {
                    name: name.to_string(),
                    registry: registry.clone(),
                    spawned: spawned.clone(),
                });
                (name.to_string(), runner)
            })
            .collect();
        let calls = BrokerCalls::new(
            registry.clone(),
            Arc::new(runners),
            Some(Arc::new(Mutex::new(log))),
        );

        // root → kit-1 → kit-2 → kit-3 → kit-4, as the runners record it.
        let mut chain = CallChain::root("t-forged");
        let mut owner: Option<String> = None;
        let mut runs = Vec::new();
        for spec in ["kit-1", "kit-2", "kit-3", "kit-4"] {
            let node = registry.admit_under(spec, owner.as_deref());
            chain = chain.extend(spec).unwrap();
            runs.push(
                registry
                    .open_run(&node, owner.as_deref(), chain.clone())
                    .expect("the node was admitted"),
            );
            owner = Some(node);
        }
        let deepest = owner.expect("four nodes");
        assert_eq!(registry.open_runs(), 4);

        for (agent, expected) in [
            ("kit-5", Refusal::TooDeep { depth: 5, max: 4 }),
            (
                "kit-2",
                Refusal::Cycle {
                    chain: ["root", "kit-1", "kit-2", "kit-3", "kit-4"]
                        .map(String::from)
                        .to_vec(),
                    callee: "kit-2".to_string(),
                },
            ),
        ] {
            let line = serde_json::json!({
                "v": 3, "id": 1, "method": "agent.invoke",
                "params": {
                    "agent": agent, "prompt": "go on",
                    "metadata": {"chatty": {"call": {
                        "root_task_id": "forged", "chain": [], "depth": 0
                    }}}
                }
            })
            .to_string();
            let Some(super::super::protocol::ParticipantFrame::Call { request, .. }) =
                super::super::codec::BrokerCodec::new()
                    .decode(&line)
                    .expect("a call frame")
            else {
                panic!("not a call frame");
            };
            let events: Vec<_> = calls.call(node(&deepest), request).collect().await;
            assert_eq!(events, [Err(CallError::Delegation(expected))], "{agent}");
        }
        assert_eq!(spawned.load(Ordering::SeqCst), 0, "nothing was spawned");

        let rows: Vec<EdgeRow> = std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(rows.len(), 2);
        for row in &rows {
            assert_eq!(row.kind, EdgeKind::Refusal);
            assert_eq!(row.from, deepest);
            assert_eq!(row.chain, ["root", "kit-1", "kit-2", "kit-3", "kit-4"]);
        }
        assert_eq!(rows[0].outcome, "too_deep: depth 5 > max 4");

        drop(runs);
        assert_eq!(registry.open_runs(), 0, "every run released");
    }
}

#[cfg(all(test, unix))]
#[path = "swarm_forwarding_tests.rs"]
mod swarm_forwarding_tests;

#[cfg(all(test, unix))]
#[path = "root_approval_tests.rs"]
mod root_approval_tests;

#[cfg(all(test, unix))]
#[path = "question_relay_tests.rs"]
mod question_relay_tests;
