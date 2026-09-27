//! What `invoke_agent`, `list_agents` and `send_message` call.
//!
//! The root process holds a direct handle; a worker holds its broker-made
//! connection (BI-4). Either way a call is one [`CallRequest`] answered by a
//! [`CallStream`]: zero or more progress events, then exactly one result or
//! error. These shapes are also the `call` / `call_progress` /
//! `call_result` / `call_error` frames of participant protocol v2.

use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// `invoke_agent`'s arguments as they cross the fabric.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvokeAgentParams {
    pub agent: String,
    pub prompt: String,
    /// A live handle to resume instead of starting a fresh worker
    /// (resumable conversations); `None` starts a new one.
    #[serde(default)]
    pub handle: Option<String>,
    #[serde(default)]
    pub include_trace: bool,
}

/// `send_message`'s arguments as they cross the fabric (tree messages).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendMessageParams {
    pub to: String,
    pub text: String,
}

/// One call. Serialises as `{"method": …, "params": …}`, the body of a v2
/// `call` frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    /// [`Transport::answer`] on `task`. Only a transport that can carry the
    /// answer back down yields this: the root's direct handle does, a
    /// worker's connection does not yet (BI-5 relays questions across
    /// hops).
    InputRequired { task: String, request: Value },
    /// The call's result. The last item of a successful stream.
    Result(Value),
}

/// What `invoke_agent` returns when the callee's task ended, whether it
/// succeeded or not: the `call_result` of an `invoke_agent` call.
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
}

/// Why a call failed. Serialises as `{"kind": …, "message": …}`, the
/// `error` of a v2 `call_error` frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "kind", content = "message", rename_all = "snake_case")]
pub enum CallError {
    #[error("unknown agent: {0}")]
    UnknownAgent(String),
    /// The root clamped a spawn context that reached outside the caller's
    /// tree or roster.
    #[error("spawn context refused: {0}")]
    SpawnContextRefused(String),
    /// Refused by policy (not on the tree, over an allowance, a cap).
    #[error("refused: {0}")]
    Refused(String),
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
            serde_json::to_value(CallError::SpawnContextRefused("outside tree".into())).unwrap(),
            json!({"kind": "spawn_context_refused", "message": "outside tree"})
        );
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
