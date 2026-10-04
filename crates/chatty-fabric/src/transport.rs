//! What `invoke_agent`, `list_agents` and `send_message` call.
//!
//! The root process holds a direct handle; a worker holds its broker-made
//! connection (BI-4). Either way a call is one [`CallRequest`] answered by a
//! [`CallStream`]: zero or more progress events, then exactly one result or
//! error. On a worker's connection these are the `agent.invoke` /
//! `agent.list` / `mailbox.post` requests, `req.progress`, and the request's
//! result or error of participant protocol v3 (ADR-0021).

use futures::stream::BoxStream;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::Remaining;
use crate::wire::{AgentEntry, TaskMetadata, WireProgress};

/// `invoke_agent`'s arguments as they cross the fabric.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
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
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RefusalReason {
    /// No rule lets the sender message the recipient: it is neither the
    /// sender's owner nor, mid-run, the sender's own child (TM-5).
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
/// method (`agent.invoke`, `agent.list`, `mailbox.post`, `mailbox.take`,
/// `module.call`)
/// with these params ([`wire::WorkerRequest`](crate::wire::WorkerRequest)).
#[derive(Debug, Clone, PartialEq)]
pub enum CallRequest {
    InvokeAgent(InvokeAgentParams),
    ListAgents,
    SendMessage(SendMessageParams),
    /// `mailbox.take` (TM-5): a node, between two of its tool rounds, takes
    /// the messages waiting on its mid-run list.
    TakeMessages,
    /// `module.call` (ADR-0024 § 2): one tool call of a paid plugin, run by
    /// the broker host in hive's module runtime.
    ModuleCall(crate::ModuleCallParams),
}

/// One item of a [`CallStream`].
#[derive(Debug, Clone, PartialEq)]
pub enum CallEvent {
    /// Progress on an `invoke_agent`, which chatty-core converts to its
    /// `InvokeAgentProgress` at its edge.
    Progress(WireProgress),
    /// A node anywhere under this call asked a human a question
    /// (`human.ask`, ADR-0021 § 2, EN-2b), and every caller between it and
    /// this one escalated it. Only a root call receives these: the broker
    /// relays a worker's question to the worker that called it as a
    /// broker→worker `human.ask`, never on a call's stream. `id` is the
    /// broker's, unique within it; the root answers with
    /// [`Transport::answer`]. `request.asker` is the broker's stamp of the
    /// agent that asked, and `request.origin` the third-party peer it
    /// relays, if any.
    Ask {
        id: String,
        request: crate::AskRequest,
    },
    /// The question or approval the broker delivered to this root call as
    /// `id` is over without the root's answer: the worker that asked
    /// withdrew it, or its callee ended or was cancelled (TB-7, EN-2a,
    /// EN-2b). The root's popover or card goes.
    InputWithdrawn { id: String },
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
    Result(CallResult),
}

/// What a call returns, typed by its method.
#[derive(Debug, Clone, PartialEq)]
pub enum CallResult {
    /// `invoke_agent` (`agent.invoke`): how the callee's task ended.
    Invoked(InvokeAgentOutcome),
    /// `list_agents` (`agent.list`): the agents the caller may address.
    Agents(Vec<AgentEntry>),
    /// `send_message` (`mailbox.post`): whether the message was taken.
    Posted(MessageStatus),
    /// `mailbox.take` (TM-5): the messages a node's tool round delivers,
    /// each already wrapped as untrusted data, oldest first.
    Messages(Vec<String>),
    /// `module.call`: what the paid plugin's tool answered.
    ModuleCalled(crate::ModuleCallOutcome),
}

/// A result is written as its method's result type: which one it is, the
/// reader knows from the request it answers, so nothing tags it.
impl Serialize for CallResult {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Invoked(outcome) => outcome.serialize(serializer),
            Self::Agents(agents) => agents.serialize(serializer),
            Self::Posted(status) => status.serialize(serializer),
            Self::Messages(messages) => messages.serialize(serializer),
            Self::ModuleCalled(outcome) => outcome.serialize(serializer),
        }
    }
}

/// What `invoke_agent` returns when the callee's task ended, whether it
/// succeeded or not: the result of an `agent.invoke` request.
///
/// A failure the callee reported (its turn failed, its worker could not
/// start) is an outcome, not a [`CallError`]: the caller renders it exactly
/// as it renders a failed A2A task. `metadata` is the terminal status's —
/// usage, trace, conversation and the runner's evidence ride there, as
/// they do on A2A.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InvokeAgentOutcome {
    pub success: bool,
    /// Everything the callee streamed as its answer, in order.
    #[serde(default)]
    pub response: String,
    /// Why it failed, when it did and said so.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Boxed: it is the largest part of a call's result, and most results
    /// carry none.
    pub metadata: Option<Box<TaskMetadata>>,
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

/// The typed reason a call's worker never started (AGE-822): its
/// worktree, its spawn or its registration failed. It leads the failed
/// result's `error` — `worker_start_failed: '<agent>' could not be started:
/// <why>` — and every caller up the tree passes it on unchanged, so the
/// user is shown the setup problem and no agent tries to work around it.
pub const WORKER_START_FAILED: &str = "worker_start_failed";

/// The `error` of a call whose worker for `agent` never started, `reason`
/// being why (AGE-822).
pub fn worker_start_failed(agent: &str, reason: &str) -> String {
    format!("{WORKER_START_FAILED}: '{agent}' could not be started: {reason}")
}

/// The agent and the reason of the [`worker_start_failed`] text in
/// `text`, wherever in it that is: a caller that wraps the error it got
/// (a tool's `Error: …`, a sub-leader's failed task) still carries it.
/// The reason ends at the first blank line, where a wrapper's own words
/// begin.
pub fn find_worker_start_failure(text: &str) -> Option<(&str, &str)> {
    let rest = &text[text.find(WORKER_START_FAILED)? + WORKER_START_FAILED.len()..];
    let rest = rest.strip_prefix(": '")?;
    let (agent, rest) = rest.split_once("' could not be started: ")?;
    let reason = rest.split("\n\n").next().unwrap_or(rest).trim();
    Some((agent, reason))
}

/// Why a call failed. On the wire it is a [`WireError`](crate::wire::WireError),
/// the `error` of a v3 error response; the two convert both ways.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
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
    /// The peer broke the protocol (a hello whose schema does not match).
    #[error("protocol: {0}")]
    Protocol(String),
    /// The run's root spec version is paid and its user has not accepted
    /// its bill (ADR-0024 § 5). Only the user can, outside the run.
    #[error("needs_acceptance: {spec}@{version}: the user has not accepted its bill")]
    NeedsAcceptance { spec: String, version: String },
    /// The ledger refused a fee for `item` (ADR-0024 § 7, L8). `resets_at`
    /// is the end of the cap's UTC month (RFC 3339) when `reason` is
    /// [`Cap`](crate::FeeRefusalReason::Cap).
    #[error("fee_refused: {item}: {reason}{}", resets_at.as_ref().map(|at| format!(", resets at {at}")).unwrap_or_default())]
    FeeRefused {
        item: crate::ItemRef,
        reason: crate::FeeRefusalReason,
        resets_at: Option<String>,
    },
}

/// Progress, then one result. An `Err` item ends the stream.
pub type CallStream = BoxStream<'static, Result<CallEvent, CallError>>;

// async_trait expands each async fn into one returning a `#[must_use]`
// boxed future; `call`/`answer` already return `Result`, which clippy sees
// as a double must-use with no way to attach a reason to the generated fn.
#[allow(clippy::double_must_use)]
#[async_trait::async_trait]
pub trait Transport: Send + Sync {
    async fn call(&self, req: CallRequest) -> Result<CallStream, CallError>;

    /// Answer the question the broker delivered as [`CallEvent::Ask`]
    /// `id` (EN-2b): the answers become the result of the asking worker's
    /// `human.ask`. Only the root's direct handle can: a worker is relayed
    /// questions over its own connection, never on a call's stream. The
    /// default refuses.
    async fn answer(&self, id: &str, answers: Vec<crate::Answer>) -> Result<(), CallError> {
        let _ = answers;
        Err(CallError::Failed(format!(
            "question '{id}' cannot be answered over this transport"
        )))
    }

    /// Ask the human a question this caller relays from a third-party peer
    /// it called over A2A (`human.ask` with `request.origin` set, EN-2b),
    /// and wait for the answers. Only a worker's connection carries one up;
    /// the default is `None`, and a caller with no transport to ask through
    /// asks its own human.
    async fn ask(
        &self,
        request: crate::AskRequest,
    ) -> Option<Result<Vec<crate::Answer>, CallError>> {
        let _ = request;
        None
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

    /// AGE-822: the start failure survives every wrapper a caller puts
    /// around it, and its reason stops where the wrapper's words begin.
    #[test]
    fn a_worker_start_failure_is_found_inside_a_wrapped_error() {
        let error = worker_start_failed("data-analyst", "no worktree: no commits");
        assert_eq!(
            find_worker_start_failure(&format!("Error: invoke_agent: {error}\n\nStop now.")),
            Some(("data-analyst", "no worktree: no commits"))
        );
        assert_eq!(find_worker_start_failure("the analyst crashed"), None);
    }

    #[test]
    fn message_statuses_have_the_wire_shape() {
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
        assert!(
            serde_json::from_str::<MessageStatus>(r#"{"status":"pending","id":"m","x":1}"#)
                .is_err()
        );
        let refused = CallError::Delegation(crate::Refusal::TooDeep { depth: 5, max: 4 });
        assert_eq!(refused.to_string(), "too_deep: depth 5 > max 4");
    }

    struct Echo;

    #[async_trait::async_trait]
    impl Transport for Echo {
        async fn call(&self, req: CallRequest) -> Result<CallStream, CallError> {
            match req {
                CallRequest::InvokeAgent(p) => Ok(futures::stream::iter([
                    Ok(CallEvent::Progress(WireProgress::Admitted(p.agent))),
                    Ok(CallEvent::Result(CallResult::Posted(
                        MessageStatus::Pending { id: p.prompt },
                    ))),
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
                Ok(CallEvent::Progress(WireProgress::Admitted("echo".into()))),
                Ok(CallEvent::Result(CallResult::Posted(
                    MessageStatus::Pending { id: "hi".into() }
                ))),
            ]
        );
    }
}
