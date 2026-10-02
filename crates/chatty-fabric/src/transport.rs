//! What `invoke_agent`, `list_agents` and `send_message` call.
//!
//! The root process holds a direct handle; a worker holds its broker-made
//! connection (BI-4). Either way a call is one [`CallRequest`] answered by a
//! [`CallStream`]: zero or more progress events, then exactly one result or
//! error. On a worker's connection these are the `agent.invoke` /
//! `agent.list` / `mailbox.post` requests, `req.progress`, and the request's
//! result or error of participant protocol v3 (ADR-0021).

use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::Remaining;

/// `invoke_agent`'s arguments as they cross the fabric.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InvokeAgentParams {
    pub agent: String,
    pub prompt: String,
    /// A live handle to resume instead of starting a fresh worker
    /// (resumable conversations); `None` starts a new one.
    #[serde(default)]
    pub handle: Option<String>,
    #[serde(default)]
    pub include_trace: bool,
    /// Where the callee is spawned and what it may reach (BI-5). `None`,
    /// which is what every `invoke_agent` sends, lets the broker derive it
    /// from the calling node's own context; one given is clamped to that
    /// context by the root broker and refused if it reaches outside it.
    /// Boxed: it is rarely set, and keeps [`CallRequest`] small.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spawn_context: Option<Box<SpawnContext>>,
    /// What the caller counts as left of its own budget (DP-3): its turns
    /// and dollars less what it has spent, delegated usage included, and
    /// its time as seconds. The broker only ever narrows the chain's budget
    /// with it; absent on the wire when the caller has no limit.
    #[serde(default, skip_serializing_if = "Remaining::is_unlimited")]
    pub remaining: Remaining,
    /// The run the call is made from (ADR-0023 § 1, GT-0b): the `taskId`
    /// of the `task.run` the calling worker is serving, which the broker
    /// issued when it opened that task's run. A node's call is decided
    /// under that run's chain, and refused when it names no open run of
    /// its own. The root calls from no run and names none; a worker's
    /// connection fills it in for the one task it serves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
}

/// What a spawned worker starts from: where its tree goes, what its branch
/// starts from, which agents it may call, the command that verifies its
/// work and the model endpoint it is metered on (ADR-0020, fabric spec
/// §3.4, BI-5).
///
/// One broker per root process spawns every worker, sub-leaders' children
/// included, so the context that used to live in a sub-leader's own broker
/// travels on the spawn request instead. The broker sets it from the
/// calling node's own context; a worker cannot widen it (invariant 6).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnContext {
    /// The tree the worker's own tree is made under: the caller's own tree.
    /// `None` when there is no workspace to isolate in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_root: Option<String>,
    /// The branch the worker's branch starts from and its evidence diffs
    /// against: the caller's own branch. `None` is the root's case: the
    /// workspace's `HEAD`, measured against the default branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_branch: Option<String>,
    /// The agents the worker may call, a subset of its caller's.
    #[serde(default)]
    pub roster: Vec<String>,
    /// The team's verification command for this worker (AGE-406), from the
    /// root's settings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification: Option<String>,
    /// The model endpoint the worker is metered on, from the root's
    /// settings; `None` for an unmetered agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
}

/// `send_message`'s arguments as they cross the fabric (tree messages).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendMessageParams {
    pub to: String,
    pub text: String,
}

/// What a `send_message` call returns: its result. Serialises as
/// `{"status": "pending", "id": …}` or
/// `{"status": "refused", "reason": "not_on_tree"}`.
///
/// A refusal is an answer, not a [`CallError`]: the sender's model reads it
/// as the tool's result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum MessageStatus {
    /// Accepted; it waits for the recipient's next delivery point.
    Pending {
        id: String,
    },
    Refused {
        reason: RefusalReason,
    },
}

/// Why the broker refused a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefusalReason {
    /// The recipient is not the sender's owner (nor, once resumable
    /// conversations exist, one of its live handles).
    NotOnTree,
    /// The message would take the recipient's pending list, or this
    /// sender's share of it, past its bound.
    OverAllowance,
    /// The recipient has ended.
    RecipientEnded,
}

impl RefusalReason {
    /// The wire name, e.g. `not_on_tree`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotOnTree => "not_on_tree",
            Self::OverAllowance => "over_allowance",
            Self::RecipientEnded => "recipient_ended",
        }
    }
}

impl std::fmt::Display for RefusalReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One call. On a worker's connection each variant is its own request
/// method (`agent.invoke`, `agent.list`, `mailbox.post`) with these params.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum CallRequest {
    InvokeAgent(InvokeAgentParams),
    ListAgents,
    SendMessage(SendMessageParams),
}

/// One item of a [`CallStream`].
#[derive(Debug, Clone, PartialEq)]
pub enum CallEvent {
    /// A progress event; for `invoke_agent` an `InvokeAgentProgress`, as
    /// JSON, which chatty-core converts back at its edge.
    Progress(Value),
    /// The callee parked its task on a question (`ask_user`, AGE-306).
    /// `request` is the question as the participant protocol carries it
    /// (`{id, questions}`); the caller answers with
    /// [`Transport::answer`] on `task`. The root's direct handle and a
    /// worker's connection both carry the answer back down, so a question
    /// climbs any number of hops (BI-5).
    InputRequired { task: String, request: Value },
    /// The question `task` was parked on is over without this caller's
    /// answer: its asker withdrew it, because the run below it was stopped
    /// (TB-7, AGE-749). The caller withdraws the copy it re-raised on its
    /// own store, which un-parks its own task toward its caller in turn, so
    /// the withdrawal climbs every hop to the root's human. A caller that
    /// already answered has nothing left to withdraw.
    ///
    /// For an approval (EN-2a) `task` is the broker's approval id: the
    /// worker that asked withdrew it, or its callee ended or was cancelled,
    /// and the root's card goes.
    InputWithdrawn { task: String },
    /// A node anywhere under this call is waiting on an execution or write
    /// approval (`human.approve`, ADR-0021 § 2). Only a root call receives
    /// these: the broker delivers every approval straight to the root,
    /// never to a worker. `id` is the broker's, unique within it; the root
    /// answers with [`Transport::approve`]. `request.asker` is the broker's
    /// stamp of the agent that asked.
    Approve {
        id: String,
        request: crate::ApprovalRequest,
    },
    /// A batch of what one run nested under this call did, tagged by the
    /// broker (TB-1). Only a root call receives these: a worker's calls
    /// are nested runs themselves, whose events go to the root directly.
    Swarm(crate::swarm::SwarmEvent),
    /// The call's result. The last item of a successful stream.
    Result(Value),
}

/// What `invoke_agent` returns when the callee's task ended, whether it
/// succeeded or not: the result of an `agent.invoke` request.
///
/// A failure the callee reported (its turn failed, its worker could not
/// start) is an outcome, not a [`CallError`]: the caller renders it exactly
/// as it renders a failed A2A task. `metadata` is the terminal status's —
/// usage, trace, conversation and the runner's evidence ride there, as
/// they do on A2A.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InvokeAgentOutcome {
    pub success: bool,
    /// Everything the callee streamed as its answer, in order.
    #[serde(default)]
    pub response: String,
    /// Why it failed, when it did and said so.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
    /// Messages that were waiting for the caller when this result was made
    /// (tree messages, TM-2): each one [`wrap_message`](crate::wrap_message)ped,
    /// oldest first. This result is their delivery point, so they are gone
    /// from the caller's pending list; absent from the JSON when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub messages: Vec<String>,
    /// The user stopped the callee (TB-7, AGE-749): its run and everything
    /// under it were cancelled, and the caller carries on without it.
    /// `success` is false and `error` reads [`CANCELLED_BY_USER`]. Absent
    /// from the JSON when false.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cancelled_by_user: bool,
}

/// The typed reason a stopped callee's caller reads, in its result's
/// `error` and in the tool error its model sees (TB-7, AGE-749).
pub const CANCELLED_BY_USER: &str = "cancelled_by_user";

/// Why a call failed. Serialises as `{"kind": …, "message": …}`, the
/// `error` of a v3 error response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "kind", content = "message", rename_all = "snake_case")]
pub enum CallError {
    #[error("unknown agent: {0}")]
    UnknownAgent(String),
    /// The root clamped a spawn context that reached outside the caller's
    /// own context: `field` names the part that did (`workspace_root`,
    /// `base_branch`, `roster`, `verification` or `endpoint`).
    #[error("spawn context refused: {field}: {reason}")]
    SpawnContextRefused { field: String, reason: String },
    /// Refused by policy (not on the tree, over an allowance, a cap).
    #[error("refused: {0}")]
    Refused(String),
    /// A delegation the broker refused before anything was spawned: the
    /// specs do not allow it, it closes a cycle, or it is too deep (PL-S2).
    #[error(transparent)]
    Delegation(crate::delegation::Refusal),
    #[error("cancelled: {0}")]
    Cancelled(String),
    /// The connection to the broker went away mid-call.
    #[error("disconnected: {0}")]
    Disconnected(String),
    #[error("{0}")]
    Failed(String),
}

/// Progress, then one result. An `Err` item ends the stream.
pub type CallStream = BoxStream<'static, Result<CallEvent, CallError>>;

#[async_trait::async_trait]
pub trait Transport: Send + Sync {
    async fn call(&self, req: CallRequest) -> Result<CallStream, CallError>;

    /// Answer the question a call's callee parked `task` on
    /// ([`CallEvent::InputRequired`]). `input` is the participant
    /// protocol's `{requestId, answers}`.
    ///
    /// A transport that never yields [`CallEvent::InputRequired`] has
    /// nothing to answer, which is the default.
    async fn answer(&self, task: &str, input: Value) -> Result<(), CallError> {
        let _ = input;
        Err(CallError::Failed(format!(
            "task '{task}' cannot be answered over this transport"
        )))
    }

    /// Answer the approval the broker delivered as
    /// [`CallEvent::Approve`] `id` (EN-2a). Only the root's direct handle
    /// can: a worker is never sent an approval, so it has none to answer.
    /// The default refuses.
    async fn approve(&self, id: &str, verdict: crate::ApprovalVerdict) -> Result<(), CallError> {
        let _ = verdict;
        Err(CallError::Failed(format!(
            "approval '{id}' cannot be answered over this transport"
        )))
    }

    /// The caller is starting a new run (tree messages, TM-2): take the
    /// messages waiting for it, [`wrap_message`](crate::wrap_message)ped and
    /// oldest first, to prepend to the run's user turn, and give every
    /// sender its allowance back. The root's direct handle is the one
    /// transport with a run of its own to start; a worker's next run is its
    /// next task, which the broker prepends them to itself. The default has
    /// nothing waiting.
    fn take_run_messages(&self) -> Vec<String> {
        Vec::new()
    }

    /// Stop `node` — a broker-assigned node name, or the spec of one of the
    /// root's own callees — and everything under it (TB-7, AGE-749): its
    /// caller's call ends with a [`CANCELLED_BY_USER`] result and the rest
    /// of the swarm keeps running. Only the root's direct handle can; the
    /// default refuses.
    fn cancel(&self, node: &str) -> Result<(), CallError> {
        Err(CallError::Failed(format!(
            "'{node}' cannot be stopped over this transport"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use serde_json::json;

    #[test]
    fn requests_and_errors_have_the_v2_frame_shape() {
        let invoke = CallRequest::InvokeAgent(InvokeAgentParams {
            agent: "local-coder".into(),
            prompt: "fix it".into(),
            handle: None,
            include_trace: false,
            spawn_context: None,
            remaining: Default::default(),
            run: None,
        });
        assert_eq!(
            serde_json::to_value(&invoke).unwrap(),
            json!({"method": "invoke_agent", "params": {
                "agent": "local-coder", "prompt": "fix it", "handle": null, "include_trace": false
            }})
        );
        assert_eq!(
            serde_json::to_value(CallRequest::ListAgents).unwrap(),
            json!({"method": "list_agents"})
        );
        let parsed: CallRequest = serde_json::from_value(json!({
            "method": "send_message", "params": {"to": "leader-0", "text": "done"}
        }))
        .unwrap();
        assert_eq!(
            parsed,
            CallRequest::SendMessage(SendMessageParams {
                to: "leader-0".into(),
                text: "done".into()
            })
        );
        assert_eq!(
            serde_json::to_value(MessageStatus::Pending { id: "msg-1".into() }).unwrap(),
            json!({"status": "pending", "id": "msg-1"})
        );
        for reason in [
            RefusalReason::NotOnTree,
            RefusalReason::OverAllowance,
            RefusalReason::RecipientEnded,
        ] {
            assert_eq!(
                serde_json::to_value(MessageStatus::Refused { reason }).unwrap(),
                json!({"status": "refused", "reason": reason.as_str()})
            );
        }
        assert_eq!(
            serde_json::to_value(CallError::SpawnContextRefused {
                field: "roster".into(),
                reason: "wider than the caller's".into()
            })
            .unwrap(),
            json!({"kind": "spawn_context_refused",
                   "message": {"field": "roster", "reason": "wider than the caller's"}})
        );
        let refused = CallError::Delegation(crate::Refusal::TooDeep { depth: 5, max: 4 });
        let json = serde_json::to_value(&refused).unwrap();
        assert_eq!(
            json,
            json!({"kind": "delegation",
                   "message": {"reason": "too_deep", "depth": 5, "max": 4}})
        );
        assert_eq!(serde_json::from_value::<CallError>(json).unwrap(), refused);
        assert_eq!(refused.to_string(), "too_deep: depth 5 > max 4");
    }

    struct Echo;

    #[async_trait::async_trait]
    impl Transport for Echo {
        async fn call(&self, req: CallRequest) -> Result<CallStream, CallError> {
            match req {
                CallRequest::InvokeAgent(p) => Ok(futures::stream::iter([
                    Ok(CallEvent::Progress(json!({"started": p.agent}))),
                    Ok(CallEvent::Result(json!(p.prompt))),
                ])
                .boxed()),
                _ => Err(CallError::UnknownAgent("echo".into())),
            }
        }
    }

    #[test]
    fn a_transport_is_object_safe_and_streams_progress_then_a_result() {
        let transport: std::sync::Arc<dyn Transport> = std::sync::Arc::new(Echo);
        let events: Vec<_> = futures::executor::block_on(async {
            transport
                .call(CallRequest::InvokeAgent(InvokeAgentParams {
                    agent: "echo".into(),
                    prompt: "hi".into(),
                    handle: None,
                    include_trace: false,
                    spawn_context: None,
                    remaining: Default::default(),
                    run: None,
                }))
                .await
                .unwrap()
                .collect()
                .await
        });
        assert_eq!(
            events,
            vec![
                Ok(CallEvent::Progress(json!({"started": "echo"}))),
                Ok(CallEvent::Result(json!("hi"))),
            ]
        );
    }
}
