//! The in-turn answer to an empty completion (AGE-401).
//!
//! A model turn with no text and no tool call is not an answer. It happens
//! with thinking models on Ollama, which sometimes write the tool call
//! inside the thinking channel where Ollama never surfaces it, and with any
//! model that simply stops. Left alone, rig ends the run after such a turn
//! and every surface reports success with nothing to show: headless prints
//! an empty line and exits 0, a delegated worker's silence reaches its
//! leader as "done".
//!
//! This hook rejects the first empty tool-free turn of a run with corrective
//! feedback, which rig appends to the history before calling the model
//! again — inside the same turn, so every surface gets the retry without
//! its own follow-up plumbing. A second empty turn is accepted as-is and
//! ends the run; the session's stream handler then reports it as
//! [`StreamErrorKind::EmptyCompletion`](crate::services::StreamErrorKind).

use rig_agent::agent::{AgentHook, HookContext, ModelTurnAction, ModelTurnFinished};
use rig_core::message::AssistantContent;

/// The feedback appended after the first empty completion of a run. The
/// `Agent protocol follow-up:` prefix is what
/// [`is_protocol_follow_up_text`](crate::services::is_protocol_follow_up_text)
/// matches on, which keeps it out of the transcript like every other
/// injected nudge.
pub const EMPTY_COMPLETION_FOLLOW_UP: &str = "Agent protocol follow-up: your last response was empty: no text and no tool call. \
     If you meant to call a tool, make the call now as a proper tool call, not inside your \
     reasoning. Otherwise give your final answer now.";

/// Retries the first empty tool-free model turn of a run once.
#[derive(Debug, Default, Clone, Copy)]
pub struct EmptyTurnRetry;

/// Run-scoped marker: the nudge has been spent on this run.
#[derive(Clone, Copy, Default)]
struct Nudged;

/// Whether `content` carries anything the turn can stand on: visible text or
/// a tool call. Reasoning alone does not count — that is the qwen3 failure.
pub fn has_output(content: &[AssistantContent]) -> bool {
    content.iter().any(|item| match item {
        AssistantContent::Text(text) => !text.text.trim().is_empty(),
        AssistantContent::ToolCall(_) | AssistantContent::Image(_) => true,
        AssistantContent::Reasoning(_) => false,
    })
}

impl AgentHook for EmptyTurnRetry {
    async fn on_model_turn_finished(
        &self,
        ctx: &HookContext,
        event: ModelTurnFinished<'_>,
    ) -> ModelTurnAction {
        if has_output(event.content) {
            return ModelTurnAction::continue_run();
        }
        if ctx.scratchpad().contains::<Nudged>() {
            tracing::warn!(
                turn = event.turn,
                "Model turn was empty again after the nudge; letting the run end"
            );
            return ModelTurnAction::continue_run();
        }
        tracing::warn!(
            turn = event.turn,
            "Model turn had no text and no tool call; retrying once with feedback"
        );
        ctx.scratchpad().insert(Nudged);
        ModelTurnAction::retry_with_feedback(EMPTY_COMPLETION_FOLLOW_UP)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig_core::message::{Reasoning, ToolCall, ToolCallId, ToolFunction};

    fn text(s: &str) -> AssistantContent {
        AssistantContent::text(s)
    }

    #[test]
    fn reasoning_alone_is_not_output() {
        assert!(!has_output(&[]));
        assert!(!has_output(&[text("   \n")]));
        assert!(!has_output(&[AssistantContent::Reasoning(Reasoning::new(
            "<tool_call>{\"name\":\"update_todo\"}</tool_call>"
        ))]));
    }

    #[test]
    fn text_or_a_tool_call_is_output() {
        assert!(has_output(&[text("done")]));
        assert!(has_output(&[AssistantContent::ToolCall(ToolCall::new(
            ToolCallId::try_from("call-1".to_string()).unwrap(),
            ToolFunction::new("read_file".to_string(), serde_json::json!({"path": "x"})),
        ))]));
    }

    /// The nudge is recognised as a protocol follow-up, so it stays hidden
    /// from the transcript wherever turn messages are rendered.
    #[test]
    fn follow_up_is_a_protocol_nudge() {
        assert!(crate::services::is_protocol_follow_up_text(
            EMPTY_COMPLETION_FOLLOW_UP
        ));
    }
}
