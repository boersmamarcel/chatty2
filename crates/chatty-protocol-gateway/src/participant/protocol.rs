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
//! [`chatty_fabric::SwarmEvent`]). Such an event names no node or chain:
//! whatever else it carries is dropped when it is parsed, and an item that
//! is not the worker's to report (text, usage, the end) is ignored. A task
//! without the flag gets none of them.
//!
//! ```text
//! broker      → {"v":3,"id":2,"method":"task.run","params":{"taskId":"task-…","text":"read it","swarmEvents":true}}
//! participant → {"v":3,"method":"task.event","params":{"kind":"swarm","id":2,"event":{"kind":"tool_call_started","id":"call-1","name":"read_file"}}}
//! ```

use chatty_fabric::{
    Answer, ApprovalRequest, ApprovalVerdict, AskReply, AskRequest, CallChain, CallError,
    CallRequest, ConversationScope, HandoffContract, NodeName, Remaining, RunId, SpawnContext,
    SwarmItem,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The state of one task, in A2A's vocabulary.
///
/// A2A's own spelling is kebab-case (`input-required`), and these values are
/// copied verbatim into the `status.state` field the broker serves, so the
/// serde renaming here is part of the public contract rather than a style
/// choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TaskState {
    Submitted,
    Working,
    /// The participant is blocked on a human. On a worker's connection that
    /// is a `human.ask` request, never a status (EN-2b); the state is A2A's,
    /// for an A2A peer's task.
    InputRequired,
    Completed,
    Failed,
    Canceled,
}

impl TaskState {
    /// Whether this state ends the task. A terminal status is the last
    /// update a caller sees, and the broker drops the task when it arrives.
    ///
    /// `InputRequired` is *not* terminal: the task is parked, not over.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Canceled)
    }
}

impl std::fmt::Display for TaskState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Via serde so the wire spelling can only be defined once.
        let s = serde_json::to_value(self)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_else(|| "unknown".to_string());
        f.write_str(&s)
    }
}

/// The caller's bearer token, carried to the worker that runs their task
/// (AGE-371).
///
/// A hosted worker validates it exactly as an HTTP request's bearer is
/// validated and runs the task as that user; a local worker has no use for
/// it and ignores it. It rides the task frame, never the worker's
/// environment: a microVM may serve successive users, and the identity
/// belongs to the turn, not the machine.
///
/// `Debug` prints nothing of it, and `serde` sees straight through to the
/// string — the socket between broker and worker is the one place it is
/// meant to be in the clear.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TaskBearer(String);

impl TaskBearer {
    pub fn new(token: impl Into<String>) -> Self {
        Self(token.into())
    }

    /// The token itself. Named so the read is visible at the call site.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for TaskBearer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TaskBearer([redacted])")
    }
}

/// Work for a worker: the prompt, and whose task it is.
///
/// What [`BrokerFrame::Task`] carries past its id, in one value so the
/// broker's submit path, a virtual agent's `run_task` and the worker's turn
/// all take the same thing and a bearer cannot be dropped between them.
#[derive(Debug, Clone, PartialEq)]
pub struct DelegatedTask {
    pub text: String,
    /// `None` for a caller that presented no bearer — a desktop parent
    /// delegating to a local worker. A hosted worker refuses such a task.
    pub bearer: Option<TaskBearer>,
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
            bearer: None,
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

    pub fn with_bearer(mut self, bearer: Option<TaskBearer>) -> Self {
        self.bearer = bearer;
        self
    }

    /// Ask the worker to capture its conversation at this task's terminal
    /// status (RC-0, AGE-649).
    pub fn with_capture_conversation(mut self, capture: bool) -> Self {
        self.capture_conversation = capture;
        self
    }
}

/// One skill on a participant's agent card.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ParticipantSkill {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub examples: Vec<String>,
}

/// What a participant publishes about itself in its `hello`.
///
/// `name` is the address callers reach it at (`/a2a/{name}`), and it is the
/// broker's to give: whatever a worker puts here is replaced by the name
/// the broker admitted its connection under (ADR-0020). A worker leaves it
/// empty.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ParticipantCard {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub skills: Vec<ParticipantSkill>,
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
    /// `metadata` is copied verbatim into the A2A status's `metadata` field.
    /// It is where a turn's token usage rides back: A2A has no usage concept
    /// — usage belongs to the ledger, not to the task protocol — and
    /// inventing a frame for it would put accounting in the wire format.
    /// The broker's ledger (AGE-307) reads it from there.
    Status {
        task_id: String,
        state: TaskState,
        message: Option<String>,
        metadata: Option<Value>,
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
    Event { task_id: String, event: SwarmItem },
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
    /// carrying this `taskId` and end with a terminal state. `bearer` is the
    /// caller's token when they presented one (AGE-371), absent on the wire
    /// otherwise. `captureConversation` asks the worker to attach its
    /// conversation to the terminal status (RC-0, AGE-649); absent on the
    /// wire when `false`.
    Task {
        task_id: String,
        text: String,
        bearer: Option<TaskBearer>,
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
    /// Progress on call `id`: an `InvokeAgentProgress`, as JSON.
    CallProgress { id: u64, event: Value },
    /// Call `id` is over, and this is what it returned.
    CallResult { id: u64, result: Value },
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bearer_does_not_debug_print() {
        let frame = BrokerFrame::Task {
            task_id: "t".into(),
            text: "x".into(),
            bearer: Some(TaskBearer::new("secret-token")),
            capture_conversation: false,
            spawn_context: None,
            handoff: None,
            budget: Box::default(),
            swarm_events: false,
        };
        let printed = format!("{frame:?}");
        assert!(!printed.contains("secret-token"), "{printed}");
        assert!(printed.contains("[redacted]"));
    }

    #[test]
    fn only_completed_failed_and_canceled_end_a_task() {
        assert!(TaskState::Completed.is_terminal());
        assert!(TaskState::Failed.is_terminal());
        assert!(TaskState::Canceled.is_terminal());
        assert!(!TaskState::Working.is_terminal());
        assert!(!TaskState::Submitted.is_terminal());
        assert!(
            !TaskState::InputRequired.is_terminal(),
            "a task waiting on a human is parked, not over (AGE-306)"
        );
    }

    #[test]
    fn task_state_displays_its_wire_spelling() {
        assert_eq!(TaskState::InputRequired.to_string(), "input-required");
        assert_eq!(TaskState::Working.to_string(), "working");
    }
}
