//! A turn the user hands straight to a named agent: `/agent <name> <prompt>`
//! (AGE-744).
//!
//! The model is not asked anything. The turn is exactly the `invoke_agent`
//! call the model would have made — the same tool, holding the same broker
//! connection (ADR-0020: one broker per root process) — reported as the
//! stream a model-issued call produces: the call, its progress (so the swarm
//! tree grows under it), its result, and the agent's answer as the turn's
//! text. A delegation that fails — the agent is unknown, the broker cannot
//! start, the worker exits without answering — ends the call with its error
//! and says so as the turn's text, so the row never hangs.
//!
//! What the delegated agent asks of the human while it runs — a command or
//! write approval the broker forwards to the root (EN-2a), a clarifying
//! question — lands on
//! this conversation's stores, like a model-issued call's; the stream
//! forwards their notifications as they arrive, so the card shows while the
//! call is still in flight (AGE-752).

use rig_agent::tool::{Tool, ToolContext};
use tokio::sync::mpsc;

use crate::models::clarification_store::ClarificationNotification;
use crate::models::execution_approval_store::{ApprovalNotification, ApprovalResolution};
use crate::services::StreamChunk;
use crate::services::llm_service::ResponseStream;
use crate::tools::invoke_agent_tool::{InvokeAgentArgs, InvokeAgentTool};

/// Who a [`TurnInput`](super::TurnInput) is handed to, and what they are
/// asked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Delegation {
    /// The agent's name, as `list_agents` lists it.
    pub agent: String,
    /// The task, as the agent receives it.
    pub prompt: String,
}

impl Delegation {
    /// What the user's message reads as in the transcript and in history.
    pub fn user_text(&self) -> String {
        format!("/agent {} {}", self.agent, self.prompt)
    }
}

/// The id the delegation's one tool call carries.
const CALL_ID: &str = "agent-command";

/// The turn's channels from this conversation's approval and clarification
/// stores: what reaches the human while the call runs.
pub(super) struct HumanPrompts {
    pub approvals: mpsc::UnboundedReceiver<ApprovalNotification>,
    pub resolutions: mpsc::UnboundedReceiver<ApprovalResolution>,
    pub clarifications: mpsc::UnboundedReceiver<ClarificationNotification>,
}

/// The stream a model that called `invoke_agent` once and then repeated its
/// answer would have produced. `tool` is `None` for an agent built without
/// one (a role that does not delegate).
pub(super) fn delegation_stream(
    tool: Option<InvokeAgentTool>,
    delegation: Delegation,
    prompts: HumanPrompts,
) -> ResponseStream {
    let HumanPrompts {
        mut approvals,
        mut resolutions,
        mut clarifications,
    } = prompts;
    Box::pin(async_stream::stream! {
        let arguments = serde_json::json!({
            "agent": delegation.agent,
            "prompt": delegation.prompt,
        });
        yield Ok(StreamChunk::ToolCallStarted {
            id: CALL_ID.to_string(),
            name: InvokeAgentTool::NAME.to_string(),
        });
        yield Ok(StreamChunk::ToolCallInput {
            id: CALL_ID.to_string(),
            arguments: arguments.to_string(),
        });

        let args = InvokeAgentArgs {
            agent: delegation.agent.clone(),
            prompt: delegation.prompt.clone(),
            include_trace: false,
        };
        let call = async move {
            match tool {
                Some(tool) => tool
                    .call(&mut ToolContext::new(), args)
                    .await
                    .map_err(|e| e.to_string()),
                None => Err("This conversation's agent cannot delegate.".to_string()),
            }
        };
        tokio::pin!(call);
        // A closed channel's `recv()` is `None` at once, which disables its
        // arm; the call's own arm always ends the loop.
        let outcome = loop {
            tokio::select! {
                outcome = &mut call => break outcome,
                Some(approval) = approvals.recv() => {
                    yield Ok(StreamChunk::ApprovalRequested {
                        id: approval.id,
                        command: approval.command,
                        is_sandboxed: approval.is_sandboxed,
                        detail: approval.detail,
                    });
                }
                Some(resolution) = resolutions.recv() => {
                    yield Ok(StreamChunk::ApprovalResolved {
                        id: resolution.id,
                        approved: resolution.approved,
                    });
                }
                Some(clarification) = clarifications.recv() => {
                    yield Ok(StreamChunk::ClarificationRequested {
                        id: clarification.id,
                        questions: clarification.questions,
                    });
                }
            }
        };
        match outcome {
            Ok(output) => {
                let result = serde_json::to_string(&output)
                    .unwrap_or_else(|_| output.response.clone());
                yield Ok(StreamChunk::ToolCallResult {
                    id: CALL_ID.to_string(),
                    result,
                });
                if output.success {
                    yield Ok(StreamChunk::Text(output.response));
                } else {
                    yield Ok(StreamChunk::Text(format!(
                        "⚠️ '{}' did not finish: {}",
                        delegation.agent, output.response
                    )));
                }
            }
            Err(error) => {
                yield Ok(StreamChunk::ToolCallError {
                    id: CALL_ID.to_string(),
                    error: error.clone(),
                });
                yield Ok(StreamChunk::Text(format!(
                    "⚠️ '{}' failed: {error}",
                    delegation.agent
                )));
            }
        }
        yield Ok(StreamChunk::Done);
    })
}
