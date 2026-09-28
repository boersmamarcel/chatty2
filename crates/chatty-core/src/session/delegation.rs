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

use rig_agent::tool::{Tool, ToolContext};

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

/// The stream a model that called `invoke_agent` once and then repeated its
/// answer would have produced. `tool` is `None` for an agent built without
/// one (a role that does not delegate).
pub(super) fn delegation_stream(
    tool: Option<InvokeAgentTool>,
    delegation: Delegation,
) -> ResponseStream {
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

        let outcome = match tool {
            Some(tool) => tool
                .call(
                    &mut ToolContext::new(),
                    InvokeAgentArgs {
                        agent: delegation.agent.clone(),
                        prompt: delegation.prompt.clone(),
                        include_trace: false,
                    },
                )
                .await
                .map_err(|e| e.to_string()),
            None => Err("This conversation's agent cannot delegate.".to_string()),
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
