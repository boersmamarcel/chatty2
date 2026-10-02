//! `send_message`: tell the agent that gave this worker its task something
//! besides the final answer (tree messages, TM-1).
//!
//! The call travels as a `send_message` call over the worker's broker-made
//! connection (ADR-0020, BI-4) and returns at once: `pending` with the
//! message's id, or `refused` with a [`chatty_fabric::RefusalReason`]. It
//! never starts a run and never interrupts one; the broker keeps the message
//! on the recipient's pending list until a delivery point takes it.
//!
//! Every worker with a connection is offered the tool, a leaf worker whose
//! `delegates_to` is empty included: unlike `invoke_agent` it needs no
//! delegation rights, only an owner.
//!
//! The description says what the tool does and nothing about whom else a
//! message might be about: the gate that counts messages naming other
//! workers measures demand, not instruction (fabric spec 5, rule 6).

use std::sync::Arc;

use futures::StreamExt;
use rig_agent::tool::{Tool, ToolContext, ToolExecutionError};
use serde::{Deserialize, Serialize};

use crate::tools::ToolError;
use chatty_fabric::{
    CallEvent, CallRequest, MessageStatus, SENDER_ALLOWANCE_BYTES, SendMessageParams, Transport,
};

/// The tool's description, as the model reads it.
pub const DESCRIPTION: &str = "Send a short message to the agent that gave you your current \
task, without waiting for a reply. The call returns at once. `pending` means the message was \
accepted: that agent reads it the next time it continues, and it neither interrupts that agent \
nor starts any work by itself. `refused` gives the reason: `not_on_tree` (you cannot message \
that name), `over_allowance` (more text than that agent can take for now; the whole message is \
refused, never shortened) or `recipient_ended` (that agent has finished). Your final answer reaches that agent \
as it always does.";

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SendMessageArgs {
    pub to: String,
    pub text: String,
}

/// What the model reads back: `{"status": "pending", "id": …}` or
/// `{"status": "refused", "reason": …}`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct SendMessageOutput {
    #[serde(flatten)]
    pub status: MessageStatus,
}

/// `send_message` over a worker's connection to its broker.
#[derive(Clone)]
pub struct SendMessageTool {
    transport: Arc<dyn Transport>,
    /// The name the broker gave this worker's owner, which `to` must be.
    owner: String,
}

impl SendMessageTool {
    /// `owner` is the name the broker's `welcome` gave this worker's owner
    /// ([`chatty_fabric::ROOT_NAME`] when the root owns it).
    pub fn new(transport: Arc<dyn Transport>, owner: impl Into<String>) -> Self {
        Self {
            transport,
            owner: owner.into(),
        }
    }
}

impl Tool for SendMessageTool {
    const NAME: &'static str = "send_message";
    type Error = ToolError;
    type Args = SendMessageArgs;
    type Output = SendMessageOutput;

    fn description(&self) -> String {
        DESCRIPTION.to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "to": {
                    "type": "string",
                    "description": format!(
                        "The recipient's name. The agent that gave you your current task is \
                         named `{}`.",
                        self.owner
                    )
                },
                "text": {
                    "type": "string",
                    "description": format!(
                        "The message. At most {} KB per run of the recipient.",
                        SENDER_ALLOWANCE_BYTES / 1024
                    )
                }
            },
            "required": ["to", "text"]
        })
    }

    fn map_error(&self, error: Self::Error) -> ToolExecutionError {
        crate::tools::map_tool_error(Self::NAME, error)
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let failed =
            |why: String| ToolError::OperationFailed(format!("send_message failed: {why}"));
        let mut stream = self
            .transport
            .call(CallRequest::SendMessage(SendMessageParams {
                to: args.to,
                text: args.text,
            }))
            .await
            .map_err(|e| failed(e.to_string()))?;
        while let Some(event) = stream.next().await {
            match event.map_err(|e| failed(e.to_string()))? {
                CallEvent::Result(value) => {
                    let status: MessageStatus = serde_json::from_value(value)
                        .map_err(|e| failed(format!("the broker's answer did not parse: {e}")))?;
                    if let MessageStatus::Refused { reason } = &status {
                        tracing::info!(reason = %reason, "send_message refused");
                    }
                    return Ok(SendMessageOutput { status });
                }
                CallEvent::Progress(_)
                | CallEvent::InputRequired { .. }
                | CallEvent::InputWithdrawn { .. }
                | CallEvent::Approve { .. }
                | CallEvent::Swarm(_) => {}
            }
        }
        Err(failed("the broker gave no answer".to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chatty_fabric::{CallError, CallStream, RefusalReason};
    use parking_lot::Mutex;
    use std::path::Path;

    /// A broker that answers every call with `status`, and remembers it.
    struct Answers {
        status: MessageStatus,
        seen: Mutex<Vec<CallRequest>>,
    }

    #[async_trait::async_trait]
    impl Transport for Answers {
        async fn call(&self, req: CallRequest) -> Result<CallStream, CallError> {
            self.seen.lock().push(req);
            Ok(futures::stream::iter([Ok(CallEvent::Result(
                serde_json::to_value(&self.status).unwrap(),
            ))])
            .boxed())
        }
    }

    fn answering(status: MessageStatus) -> (SendMessageTool, Arc<Answers>) {
        let broker = Arc::new(Answers {
            status,
            seen: Mutex::new(Vec::new()),
        });
        (SendMessageTool::new(broker.clone(), "root"), broker)
    }

    /// Invariant 7: the description, as a golden, says what the tool does
    /// and mentions neither relaying nor siblings.
    #[test]
    fn send_message_description_is_neutral() {
        let (tool, _) = answering(MessageStatus::Pending { id: "msg-1".into() });
        let parameters = tool.parameters();
        let text = format!(
            "{}\n\nto: {}\ntext: {}\n",
            tool.description(),
            parameters["properties"]["to"]["description"]
                .as_str()
                .unwrap(),
            parameters["properties"]["text"]["description"]
                .as_str()
                .unwrap(),
        );
        for word in ["relay", "sibling"] {
            assert!(
                !text.to_lowercase().contains(word),
                "the description mentions `{word}`: {text}"
            );
        }

        let golden = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/tools/goldens/send_message_description.txt");
        if std::env::var("UPDATE_GOLDENS").is_ok() {
            std::fs::create_dir_all(golden.parent().unwrap()).unwrap();
            std::fs::write(&golden, &text).unwrap();
            return;
        }
        let expected = std::fs::read_to_string(&golden).unwrap_or_else(|_| {
            panic!(
                "missing golden {}; re-run with UPDATE_GOLDENS=1",
                golden.display()
            )
        });
        assert_eq!(
            expected, text,
            "the send_message description changed; it must stay neutral (fabric spec 5, \
             rule 6). If the change is deliberate, re-run with UPDATE_GOLDENS=1"
        );
    }

    #[tokio::test]
    async fn the_call_goes_to_the_broker_and_its_answer_is_the_output() {
        let (tool, broker) = answering(MessageStatus::Pending { id: "msg-7".into() });
        let out = tool
            .call(
                &mut ToolContext::new(),
                SendMessageArgs {
                    to: "root".into(),
                    text: "halfway".into(),
                },
            )
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_value(&out).unwrap(),
            serde_json::json!({"status": "pending", "id": "msg-7"})
        );
        assert_eq!(
            broker.seen.lock().as_slice(),
            [CallRequest::SendMessage(SendMessageParams {
                to: "root".into(),
                text: "halfway".into()
            })]
        );

        for reason in [
            RefusalReason::NotOnTree,
            RefusalReason::OverAllowance,
            RefusalReason::RecipientEnded,
        ] {
            let (tool, _) = answering(MessageStatus::Refused { reason });
            let out = tool
                .call(
                    &mut ToolContext::new(),
                    SendMessageArgs {
                        to: "x".into(),
                        text: "y".into(),
                    },
                )
                .await
                .expect("a refusal is an answer, not a tool error");
            assert_eq!(
                serde_json::to_value(&out).unwrap(),
                serde_json::json!({"status": "refused", "reason": reason.as_str()})
            );
        }
    }
}
