//! What a worker sends the broker (ADR-0021 § 1).
//!
//! | Requests | Notifications | Results |
//! |---|---|---|
//! | `session.hello`, `agent.invoke`, `agent.list`, `mailbox.post`, `mailbox.take`, `module.call`, `human.ask`, `human.approve` | `task.event`, `req.cancel` | `task.run` ([`TaskOutcome`]), a relayed `human.ask` ([`AskReply`](crate::AskReply)) |

use std::borrow::Cow;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

use super::{DecodeError, IdParams, TaskMetadata, TaskState, no_params, params};
use crate::{
    ApprovalRequest, AskRequest, InvokeAgentParams, ModuleCallParams, SendMessageParams, SwarmItem,
};

/// A request a worker may make.
#[derive(Debug)]
pub enum WorkerRequest {
    /// `session.hello`: the first message on a connection, and only then.
    SessionHello(HelloParams),
    /// `agent.invoke`
    AgentInvoke(InvokeAgentParams),
    /// `agent.list`
    AgentList,
    /// `mailbox.post`
    MailboxPost(SendMessageParams),
    /// `mailbox.take` (TM-5): the messages waiting on the worker's mid-run
    /// list, taken between two of its tool rounds. No params.
    MailboxTake,
    /// `module.call` (ADR-0024 § 2, MK-2): one tool call of a paid plugin
    /// the worker's lockfile pins.
    ModuleCall(ModuleCallParams),
    /// `human.approve` (EN-2a): an approval only the root answers.
    HumanApprove(ApprovalRequest),
    /// `human.ask` (EN-2b): a question, relayed up the caller chain.
    HumanAsk(AskRequest),
}

impl WorkerRequest {
    /// The method `method` with params `raw`; a method a worker may not
    /// send does not decode.
    pub fn decode(method: &str, raw: Option<&RawValue>) -> Result<Self, DecodeError> {
        Ok(match method {
            "session.hello" => Self::SessionHello(params(method, raw)?),
            "agent.invoke" => Self::AgentInvoke(params(method, raw)?),
            "agent.list" => {
                no_params(method, raw)?;
                Self::AgentList
            }
            "mailbox.post" => Self::MailboxPost(params(method, raw)?),
            "mailbox.take" => {
                no_params(method, raw)?;
                Self::MailboxTake
            }
            "module.call" => Self::ModuleCall(params(method, raw)?),
            "human.approve" => Self::HumanApprove(params(method, raw)?),
            "human.ask" => Self::HumanAsk(params(method, raw)?),
            other => return Err(DecodeError::WrongDirection(other.to_string())),
        })
    }
}

/// A notification a worker may send.
#[derive(Debug)]
pub enum WorkerNotification {
    /// `task.event`: progress on a `task.run`, named by its id.
    TaskEvent(TaskEvent<'static>),
    /// `req.cancel`: the worker withdraws one of its own requests.
    ReqCancel(IdParams),
}

impl WorkerNotification {
    /// The notification `method` with params `raw`.
    pub fn decode(method: &str, raw: Option<&RawValue>) -> Result<Self, DecodeError> {
        Ok(match method {
            "task.event" => Self::TaskEvent(params(method, raw)?),
            "req.cancel" => Self::ReqCancel(params(method, raw)?),
            other => return Err(DecodeError::WrongDirection(other.to_string())),
        })
    }
}

/// `session.hello`'s params.
///
/// `schema` is the SHA-256 (hex) of this build's canonical wire schema
/// export (ADR-0021 § 1, EN-3b): [`crate::wire::schema::hash`]. The broker
/// refuses a hello whose `schema` does not match its own with
/// `error{kind: protocol}`, then closes the connection.
#[derive(Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HelloParams {
    #[serde(default)]
    pub card: ParticipantCard,
    #[serde(default)]
    pub schema: String,
}

/// One skill on a participant's agent card.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ParticipantSkill {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub examples: Vec<String>,
}

/// What a participant publishes about itself in its `session.hello`.
///
/// `name` is the address callers reach it at, and it is the broker's to
/// give: whatever a worker puts here is replaced by the name the broker
/// admitted its connection under (ADR-0020). A worker leaves it empty.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
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

/// `task.event`: one update on a `task.run`, named by that request's id.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TaskEvent<'a> {
    /// A non-terminal status. A terminal one is the `task.run`'s result,
    /// and only that carries metadata.
    Status {
        id: u64,
        state: TaskState,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<Cow<'a, str>>,
    },
    /// A chunk of the task's output.
    #[serde(rename_all = "camelCase")]
    Artifact {
        id: u64,
        text: Cow<'a, str>,
        #[serde(default)]
        last_chunk: bool,
    },
    /// One of the worker's turns or tool events (TB-1).
    Swarm {
        id: u64,
        event: Cow<'a, WorkerSwarmItem>,
    },
}

/// `task.run`'s result: how the task ended.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TaskOutcome<'a> {
    pub state: TaskState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<Cow<'a, str>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Cow<'a, TaskMetadata>>,
}

/// What a worker may report about its own run on a task sent with
/// `swarmEvents` (TB-1): its turns and its tool events. Text, usage and the
/// end are the broker's to read off the task's own frames, so a worker
/// cannot send them, and the node and chain are the broker's to stamp.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkerSwarmItem {
    TurnStarted,
    ToolCallStarted { id: String, name: String },
    ToolCallResult { id: String, result: String },
    ToolCallError { id: String, error: String },
}

impl From<WorkerSwarmItem> for SwarmItem {
    fn from(item: WorkerSwarmItem) -> Self {
        match item {
            WorkerSwarmItem::TurnStarted => Self::TurnStarted,
            WorkerSwarmItem::ToolCallStarted { id, name } => Self::ToolCallStarted { id, name },
            WorkerSwarmItem::ToolCallResult { id, result } => Self::ToolCallResult { id, result },
            WorkerSwarmItem::ToolCallError { id, error } => Self::ToolCallError { id, error },
        }
    }
}
