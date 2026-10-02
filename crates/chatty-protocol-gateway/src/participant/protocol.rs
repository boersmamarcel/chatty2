//! What a local participant and the broker say to each other over the
//! socket, in the broker's own terms.
//!
//! [`ParticipantFrame`] is everything a worker says and [`BrokerFrame`]
//! everything the broker says. They are not the wire: the per-connection
//! [`FrameCodec`](super::codec::FrameCodec) puts each one in ADR-0021's v3
//! envelope and takes it back out (see [`super::codec`] for the envelope,
//! the method table and the id rules). One JSON object per line, in both
//! directions.
//!
//! The frames are deliberately *not* A2A: A2A is the broker's public wire
//! format, and a child process is not a public endpoint. The broker maps
//! between the two (see [`super::registry`]).
//!
//! A connection is made by the broker, not by the worker: the broker admits
//! a node, creates a socket pair, keeps one end and hands the other to the
//! child it spawns. The connection *is* the identity, so the worker's
//! `session.hello` names nothing — a card's `name` is ignored — and its
//! result tells the worker who it is.
//!
//! # A session
//!
//! ```text
//! participant → {"v":3,"id":1,"method":"session.hello","params":{"card":{"name":"",…}}}
//! broker      → {"v":3,"id":1,"result":{"name":"local-coder-0","scope":"root","owner":null}}
//! broker      → {"v":3,"id":1,"method":"task.run","params":{"taskId":"task-…","text":"summarise foo.rs"}}
//! participant → {"v":3,"method":"task.event","params":{"kind":"status","id":1,"state":"working","message":"read_file"}}
//! participant → {"v":3,"method":"task.event","params":{"kind":"artifact","id":1,"text":"foo.rs defines…","lastChunk":false}}
//! participant → {"v":3,"id":1,"result":{"state":"completed"}}
//! ```
//!
//! Each side numbers its own requests, so the worker's hello and the
//! broker's first `task.run` may both be `1`. A `task.event` names its
//! task by the `task.run`'s id; the task's A2A `taskId` rides only in
//! `task.run`'s params. The broker stops a task with `req.cancel` naming
//! that `task.run`.
//!
//! # Calls (BI-4, AGE-636)
//!
//! A worker reaches other agents over the same connection: its
//! `invoke_agent`, `list_agents` and `send_message` are `agent.invoke`,
//! `agent.list` and `mailbox.post` requests, and the broker runs each as the
//! node this connection names — the request says nothing about who is
//! calling. Several can be in flight within one task, and their answers
//! interleave in whatever order they finish.
//!
//! ```text
//! participant → {"v":3,"id":2,"method":"agent.invoke",
//!                "params":{"agent":"local-reviewer","prompt":"review it","handle":null,"include_trace":false}}
//! participant → {"v":3,"id":3,"method":"agent.list"}
//! broker      → {"v":3,"id":3,"result":[{"name":"local-reviewer","origin":"local",…}]}
//! broker      → {"v":3,"method":"req.progress","params":{"id":2,"event":{"Step":"read_file"}}}
//! broker      → {"v":3,"method":"req.progress","params":{"id":2,"event":{"Text":"Looks good."}}}
//! broker      → {"v":3,"id":2,"result":{"success":true,"response":"Looks good.","metadata":{"usage":[…]}}}
//! ```
//!
//! `event` is an `InvokeAgentProgress` as JSON (`Step` for a line about the
//! callee's work, `Text` for its answer as it streams). A call that cannot
//! run at all ends with an `error` (`{kind, message}`); a callee whose task
//! failed ends with a result whose `success` is false, exactly as a failed
//! A2A task. When the worker's connection closes, the broker cancels every
//! call still in flight on it, which reaps the workers those calls started.
//!
//! # A question (EN-2b, AGE-771)
//!
//! A worker whose `ask_user` waits on someone parks its task on a
//! `human.ask` request and gets the answers as its result. The broker
//! stamps the asker from the connection (whatever the worker put there is
//! overwritten) and relays the request, under an id of its own, to whoever
//! called the asking worker: a worker gets it as a broker→worker
//! `human.ask`, the root as [`chatty_fabric::CallEvent::Ask`]. A worker
//! that cannot answer it answers `escalate`, and the broker forwards the
//! original request, first stamp intact, to the next caller up, so the root
//! sees the agent that asked and never a relayer's name. A worker's model
//! has no human, so it escalates every question relayed to it.
//!
//! ```text
//! leaf   → {"v":3,"id":2,"method":"human.ask","params":{"questions":[{"id":"q1","question":"Which database?","options":["SQLite"]}]}}
//! broker → {"v":3,"id":3,"method":"human.ask","params":{"question":"question-1","request":{"questions":[…],
//!           "asker":{"agent":"leaf-0","chain":["mid","leaf"]}}}}            (to mid, the leaf's caller)
//! mid    → {"v":3,"id":3,"result":"escalate"}
//! broker → {"v":3,"id":2,"result":[{"id":"q1","answer":"SQLite","custom":false}]}   (to leaf, once the root answered)
//! ```
//!
//! A question ends without an answer when the worker withdraws it
//! (`req.cancel`) or its callee ends or is cancelled, which closes its
//! connection: the broker sends `req.cancel` for the copy it relayed to a
//! worker, or withdraws the root's popover
//! ([`chatty_fabric::CallEvent::InputWithdrawn`], by the broker's id).
//!
//! # An approval (EN-2a, AGE-770)
//!
//! A worker whose command or write needs a human asks the root, and only the
//! root: it sends a `human.approve` request and waits for its result. The
//! broker stamps the asker from the connection (whatever the worker put
//! there is overwritten), delivers it to the root's call under an id of its
//! own ([`chatty_fabric::CallEvent::Approve`]), and sends the root's answer
//! back as the request's result. No caller in between sees it, and the
//! broker never sends `human.approve` to a worker. A worker that stops
//! waiting withdraws it with `req.cancel`.
//!
//! ```text
//! participant → {"v":3,"id":4,"method":"human.approve","params":{"kind":"exec","command_or_path":"[shell] echo hi"}}
//! broker      → {"v":3,"id":4,"result":"approved"}
//! ```
//!
//! # A nested run's events (TB-1, AGE-663)
//!
//! A task a worker's call started — a run nested under the root's callee —
//! is sent with `"swarmEvents":true` when the root is listening. Its worker
//! then reports its turns and tool events as `task.event`s of kind `swarm`
//! beside the usual ones, and the broker forwards them to the root tagged
//! with the node and chain from its own task table (see
//! [`chatty_fabric::SwarmEvent`]). Such an event names no node or chain,
//! and carries only what is the worker's to report
//! ([`WorkerSwarmItem`]): a forged tag, or text, usage or the end, does not
//! decode. A task without the flag gets none of them.
//!
//! ```text
//! broker      → {"v":3,"id":2,"method":"task.run","params":{"taskId":"task-…","text":"read it","swarmEvents":true}}
//! participant → {"v":3,"method":"task.event","params":{"kind":"swarm","id":2,"event":{"kind":"tool_call_started","id":"call-1","name":"read_file"}}}
//! ```

use chatty_fabric::wire::{TaskIdentity, TaskMetadata, WorkerSwarmItem};
use chatty_fabric::{
    Answer, ApprovalRequest, ApprovalVerdict, AskReply, AskRequest, CallChain, CallError,
    CallRequest, CallResult, ConversationScope, HandoffContract, NodeName, Remaining, RunId,
    SpawnContext,
};

pub use chatty_fabric::wire::{ParticipantCard, ParticipantSkill, TaskState};

/// Work for a worker: the prompt, and whose task it is.
///
/// What [`BrokerFrame::Task`] carries past its id, in one value so the
/// broker's submit path, a virtual agent's `run_task` and the worker's turn
/// all take the same thing and a bearer cannot be dropped between them.
#[derive(Debug, Clone, PartialEq)]
pub struct DelegatedTask {
    pub text: String,
    /// Whose task it is, as the host edge stamped it (ADR-0021 § 2); `None`
    /// for a desktop root's task. A hosted worker refuses a task without
    /// one.
    pub identity: Option<TaskIdentity>,
    /// Whether the worker should capture its conversation at this task's
    /// terminal status (RC-0, AGE-649). Opt-in and off by default, so an
    /// ordinary task's frames are unchanged.
    pub capture_conversation: bool,
    /// Where the worker is spawned and what it may reach (BI-5), already
    /// clamped by the broker. `None` leaves a runner to its own defaults:
    /// the root's workspace and settings.
    pub spawn_context: Option<SpawnContext>,
    /// The role and JSON Schema the worker's final answer must match (TD-2,
    /// AGE-693). Set by the runner of a role the team names a schema for;
    /// `None` otherwise, and then absent from the task frame.
    pub handoff: Option<HandoffContract>,
    /// The run a broker call starts: who called, from which of its runs,
    /// and the chain the gate granted (DP-2, ADR-0023 § 1). Stays with the
    /// broker, like `caller`, and is not part of the task frame: the
    /// registry opens the task's run under it in the critical section that
    /// queues the `task.run`, so the worker's first call already finds it.
    /// A task without one is refused there: every task a node is handed
    /// opens a run.
    pub call: Option<CallStamp>,
    /// What the worker may spend on this task (DP-3), as the task frame
    /// carries it: the broker fills the frame from `call`'s chain when it
    /// hands the task over, and a worker reads it back here. Unlimited for
    /// a task no broker call started.
    pub budget: Remaining,
    /// Ask the worker for its turns and tool events as `event` frames (TB-1):
    /// set by the broker on a nested run whose root is listening.
    pub swarm_events: bool,
}

/// What a broker call stamps on the task it starts (DP-2): the gate's
/// grant (ADR-0023 § 1).
#[derive(Debug, Clone, PartialEq)]
pub struct CallStamp {
    /// The calling node's name; `None` for the in-process root.
    pub caller: Option<String>,
    /// The caller's run the call was made from, which the call named
    /// (GT-0b); `None` for the root. The task's run opens under it only
    /// while it is still the caller's open run.
    pub from_run: Option<RunId>,
    /// The caller's chain plus the callee, built from the broker's own
    /// task table.
    pub chain: CallChain,
}

#[cfg(test)]
impl DelegatedTask {
    /// A task the root hands a node directly, stamped as a granted root
    /// call would stamp it (ADR-0023 § 1): what a fixture with no gate in
    /// front of it submits, so the task opens its run.
    pub(crate) fn from_root(text: impl Into<String>) -> Self {
        Self::new(text).with_call(Some(CallStamp {
            caller: None,
            from_run: None,
            chain: CallChain::root("t-fixture")
                .extend("fixture")
                .expect("a chain of one"),
        }))
    }
}

impl DelegatedTask {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            identity: None,
            capture_conversation: false,
            spawn_context: None,
            handoff: None,
            call: None,
            budget: Remaining::default(),
            swarm_events: false,
        }
    }

    /// Ask the worker for its `event` frames (TB-1).
    pub fn with_swarm_events(mut self, swarm_events: bool) -> Self {
        self.swarm_events = swarm_events;
        self
    }

    /// The handoff contract the worker's answer must meet (TD-2).
    pub fn with_handoff(mut self, handoff: Option<HandoffContract>) -> Self {
        self.handoff = handoff;
        self
    }

    /// The budget a worker read off its task frame (DP-3).
    pub fn with_budget(mut self, budget: Remaining) -> Self {
        self.budget = budget;
        self
    }

    /// The budget the task frame carries (DP-3): what the call's chain
    /// leaves the worker now, or the task's own when no broker call started
    /// it.
    pub fn frame_budget(&self) -> Remaining {
        match self.call.as_ref() {
            Some(call) => call.chain.left_at(std::time::SystemTime::now()),
            None => self.budget.clone(),
        }
    }

    /// The run a broker call starts (DP-2).
    pub fn with_call(mut self, call: Option<CallStamp>) -> Self {
        self.call = call;
        self
    }

    /// The context the worker is spawned with (BI-5).
    pub fn with_spawn_context(mut self, context: Option<SpawnContext>) -> Self {
        self.spawn_context = context;
        self
    }

    /// Whose task it is (ADR-0021 § 2).
    pub fn with_identity(mut self, identity: Option<TaskIdentity>) -> Self {
        self.identity = identity;
        self
    }

    /// Ask the worker to capture its conversation at this task's terminal
    /// status (RC-0, AGE-649).
    pub fn with_capture_conversation(mut self, capture: bool) -> Self {
        self.capture_conversation = capture;
        self
    }
}

/// A frame from a participant to the broker.
#[derive(Debug, Clone)]
pub enum ParticipantFrame {
    /// `session.hello`: the first message on a connection. A second one
    /// is a protocol error. The card's `name` is ignored: the connection
    /// already names the node.
    Hello { card: ParticipantCard },
    /// A task moved, optionally with progress text. The broker turns this
    /// into an A2A `TaskStatusUpdateEvent`. On the wire a terminal one is
    /// the `task.run`'s result and any other a `task.event` of kind
    /// `status`; a task waiting on a human sends [`Self::Ask`] instead.
    ///
    /// `metadata` is a terminal status's only (the `task.run` result's): a
    /// turn's token usage, its trace and whatever else the task reports
    /// ([`TaskMetadata`]). The broker's ledger (AGE-307) reads the usage
    /// from there. A non-terminal status with metadata does not encode.
    Status {
        task_id: String,
        state: TaskState,
        message: Option<String>,
        metadata: Option<TaskMetadata>,
    },
    /// A chunk of the task's output, in stream order. The broker turns this
    /// into an A2A `TaskArtifactUpdateEvent`.
    Artifact {
        task_id: String,
        text: String,
        last_chunk: bool,
    },
    /// A call to another agent, made as the node this connection names
    /// (BI-4): an `agent.invoke`, `agent.list` or `mailbox.post` request.
    /// `id` is the worker's own call id on its side and the request id the
    /// worker gave it on the broker's; the codec maps between them.
    Call { id: u64, request: CallRequest },
    /// One of the worker's own turns or tool events on a task sent with
    /// `swarmEvents` (TB-1). The broker tags it; the frame cannot.
    Event {
        task_id: String,
        event: WorkerSwarmItem,
    },
    /// The worker withdraws call `id` (`req.cancel`): the broker stops it,
    /// and sends nothing more for it.
    CancelCall { id: u64 },
    /// A `human.approve` request (EN-2a): the worker waits on the root's
    /// answer, [`BrokerFrame::Approval`]. `id` is the worker's own approval
    /// number on its side and the request id on the broker's; the codec
    /// maps between them.
    Approve { id: u64, request: ApprovalRequest },
    /// The worker withdraws approval `id` (`req.cancel`): nobody is waiting
    /// on it any more.
    CancelApproval { id: u64 },
    /// A `human.ask` request (EN-2b): the worker parks its task on a
    /// question and waits for the answers, [`BrokerFrame::Answer`]. `id` is
    /// the worker's own question number on its side and the request id on
    /// the broker's; the codec maps between them.
    Ask { id: u64, request: AskRequest },
    /// The worker withdraws question `id` (`req.cancel`): nobody is waiting
    /// on its answers any more.
    CancelAsk { id: u64 },
    /// The worker's result to the question the broker relayed to it as
    /// `question` ([`BrokerFrame::Ask`]): the answers, or `escalate`.
    AskReply { question: String, reply: AskReply },
}

/// A frame from the broker to a participant.
#[derive(Debug, Clone)]
pub enum BrokerFrame {
    /// The answer to `hello`: who this connection is. `name` is what
    /// callers address, `scope` the conversation the node works for, and
    /// `owner` the node that asked for it (`None` when the root did).
    Welcome {
        name: NodeName,
        scope: ConversationScope,
        owner: Option<NodeName>,
    },
    /// The `session.hello` is refused (any hello on the shared socket): an
    /// `error` for the hello's id, after which the broker closes the
    /// connection. Without a pending hello nothing is sent.
    Error { reason: String },
    /// Work: a `task.run` request. Answer with `Status` / `Artifact` frames
    /// carrying this `taskId` and end with a terminal state. `identity` is
    /// whose task it is, absent on the wire for a desktop root's task.
    /// `captureConversation` asks the worker to attach its
    /// conversation to the terminal status (RC-0, AGE-649); absent on the
    /// wire when `false`.
    Task {
        task_id: String,
        text: String,
        identity: Option<TaskIdentity>,
        capture_conversation: bool,
        /// The context this worker was spawned with (BI-5): its workspace
        /// root, base branch, roster, verification command and endpoint.
        /// Absent on the wire for a task given without one.
        spawn_context: Option<SpawnContext>,
        /// The role and schema the worker's final answer must match (TD-2,
        /// AGE-693). Absent on the wire for a role without one.
        handoff: Option<HandoffContract>,
        /// What the worker may spend on this task (DP-3): the turns,
        /// seconds and dollars its call chain leaves it. The worker runs
        /// under the tighter of this and its own spec's budget. Absent on the
        /// wire when unlimited.
        /// Boxed: the frame enum stays small for the frames that are not
        /// a task.
        budget: Box<Remaining>,
        /// Report turns and tool events as `swarm` task events (TB-1).
        /// Absent on the wire when `false`.
        swarm_events: bool,
    },
    /// The caller went away: `req.cancel` of the task's `task.run`. Stop
    /// working on `taskId`.
    Cancel { task_id: String },
    /// Progress on call `id`.
    CallProgress {
        id: u64,
        event: chatty_fabric::wire::WireProgress,
    },
    /// Call `id` is over, and this is what it returned.
    CallResult { id: u64, result: CallResult },
    /// Call `id` could not be carried out, and is over.
    CallError { id: u64, error: CallError },
    /// The root's answer to the worker's approval `id`: the `human.approve`
    /// request's result (EN-2a).
    Approval { id: u64, verdict: ApprovalVerdict },
    /// The result of the worker's question `id` (`human.ask`, EN-2b): the
    /// answers, or why nobody gave any.
    Answer {
        id: u64,
        answers: Result<Vec<Answer>, CallError>,
    },
    /// A question one of this worker's callees asked (a broker→worker
    /// `human.ask`, EN-2b), under the broker's id `question`, asker
    /// stamped. Answer it, or escalate it, with
    /// [`ParticipantFrame::AskReply`].
    Ask {
        question: String,
        request: AskRequest,
    },
    /// The broker withdraws the question it relayed as `question`
    /// (`req.cancel`): its asker is gone or stopped waiting.
    CancelAsk { question: String },
}
