//! What the broker sends a worker (ADR-0021 § 1).
//!
//! | Requests | Notifications | Results |
//! |---|---|---|
//! | `task.run`, `human.ask` (a callee's, relayed) | `req.progress`, `req.cancel` | `session.hello` ([`Welcome`]), `agent.invoke` ([`InvokeAgentOutcome`](crate::InvokeAgentOutcome)), `agent.list` ([`AgentEntry`]s), `mailbox.post` ([`MessageStatus`](crate::MessageStatus)), `mailbox.take` (wrapped messages), `module.call` ([`ModuleCallOutcome`](crate::ModuleCallOutcome)), `human.approve` ([`ApprovalVerdict`](crate::ApprovalVerdict)), `human.ask` ([`Answer`](crate::Answer)s), any of them an [`error`](super::WireError) |

use std::borrow::Cow;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

use super::{DecodeError, IdParams, ParticipantCard, TaskIdentity, WireProgress, params};
use crate::{
    AgentOrigin, AskRequest, ConversationScope, HandoffContract, NodeName, Remaining, SpawnContext,
};

/// A request the broker may make.
#[derive(Debug)]
pub enum BrokerRequest {
    /// `task.run`: work.
    TaskRun(Box<TaskRunParams<'static>>),
    /// `human.ask` (EN-2b): a callee's question, relayed to its caller.
    HumanAsk(RelayedAskParams<'static>),
}

impl BrokerRequest {
    /// The method `method` with params `raw`; a method the broker may not
    /// send (`human.approve` among them) does not decode.
    pub fn decode(method: &str, raw: Option<&RawValue>) -> Result<Self, DecodeError> {
        Ok(match method {
            "task.run" => Self::TaskRun(Box::new(params(method, raw)?)),
            "human.ask" => Self::HumanAsk(params(method, raw)?),
            other => return Err(DecodeError::WrongDirection(other.to_string())),
        })
    }
}

/// A notification the broker may send.
#[derive(Debug)]
pub enum BrokerNotification {
    /// `req.progress`: progress on one of the worker's requests.
    ReqProgress(ProgressParams<'static>),
    /// `req.cancel`: the broker withdraws one of its `task.run`s or relayed
    /// `human.ask`s.
    ReqCancel(IdParams),
}

impl BrokerNotification {
    /// The notification `method` with params `raw`.
    pub fn decode(method: &str, raw: Option<&RawValue>) -> Result<Self, DecodeError> {
        Ok(match method {
            "req.progress" => Self::ReqProgress(params(method, raw)?),
            "req.cancel" => Self::ReqCancel(params(method, raw)?),
            other => return Err(DecodeError::WrongDirection(other.to_string())),
        })
    }
}

/// `session.hello`'s result: who this connection is.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Welcome<'a> {
    pub name: Cow<'a, NodeName>,
    pub scope: Cow<'a, ConversationScope>,
    pub owner: Option<Cow<'a, NodeName>>,
}

/// `task.run`'s params.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskRunParams<'a> {
    /// The task's A2A id. Events and the result name the `task.run`'s
    /// request id instead.
    pub task_id: Cow<'a, str>,
    pub text: Cow<'a, str>,
    /// Whose task it is; absent for a desktop root's task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<Cow<'a, TaskIdentity>>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub capture_conversation: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spawn_context: Option<Cow<'a, SpawnContext>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handoff: Option<Cow<'a, HandoffContract>>,
    #[serde(default, skip_serializing_if = "is_unlimited")]
    pub budget: Cow<'a, Remaining>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub swarm_events: bool,
}

fn is_unlimited<B: std::ops::Deref<Target = Remaining>>(budget: &B) -> bool {
    budget.is_unlimited()
}

/// `req.progress`'s params.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProgressParams<'a> {
    pub id: u64,
    pub event: Cow<'a, WireProgress>,
}

/// A broker→worker `human.ask`'s params: the broker's id for the question
/// and the question as its asker sent it, asker stamped.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RelayedAskParams<'a> {
    pub question: Cow<'a, str>,
    pub request: Cow<'a, AskRequest>,
}

/// One agent the caller may address: an entry of `agent.list`'s result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentEntry {
    pub name: String,
    /// The card's display name, or its name when it has none.
    pub display_name: String,
    pub description: String,
    pub version: String,
    pub skills: Vec<AgentEntrySkill>,
    pub capabilities: AgentCapabilities,
    /// Where the agent runs, as the broker knows it.
    pub origin: AgentOrigin,
}

impl AgentEntry {
    /// `card`'s entry, for an agent at `origin`.
    pub fn from_card(card: &ParticipantCard, origin: AgentOrigin) -> Self {
        Self {
            name: card.name.clone(),
            display_name: card
                .display_name
                .clone()
                .unwrap_or_else(|| card.name.clone()),
            description: card.description.clone(),
            version: card.version.clone(),
            skills: card
                .skills
                .iter()
                .map(|skill| AgentEntrySkill {
                    name: skill.name.clone(),
                    description: skill.description.clone(),
                    examples: skill.examples.clone(),
                })
                .collect(),
            capabilities: AgentCapabilities { streaming: true },
            origin,
        }
    }
}

/// One skill of an [`AgentEntry`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentEntrySkill {
    pub name: String,
    pub description: String,
    pub examples: Vec<String>,
}

/// What an [`AgentEntry`]'s agent can do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentCapabilities {
    pub streaming: bool,
}
