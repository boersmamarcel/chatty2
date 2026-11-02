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
//!
//! The same hook answers a turn the provider cut off at its output cap
//! (`finish_reason: length`, AGE-936): a configured `max_tokens`, or one the
//! hosted egress lowered to what a task's budget has left; on a thinking
//! model it counts the reasoning too. The first tool-free cut-off turn of a
//! run is retried once, with feedback asking for a shorter answer or a file
//! written in parts. A second one is accepted: cut-off text stands as the
//! answer, with `llm_service::TRUNCATION_NOTE` appended so a leader or a
//! human can see it may be incomplete, and a cut-off turn with no answer at
//! all ends the run with rig's "finish_reason=Length" error, which every
//! surface stops on ([`StreamErrorKind::EmptyCompletion`]). So a cap costs a
//! run at most one extra model call, never a loop of truncated answers.
//!
//! [`StreamErrorKind::EmptyCompletion`]: crate::services::StreamErrorKind::EmptyCompletion

use rig_agent::agent::{AgentHook, HookContext, ModelTurnAction, ModelTurnFinished};
use rig_core::completion::FinishReason;
use rig_core::message::AssistantContent;

/// The feedback appended after the first empty completion of a run. The
/// `Agent protocol follow-up:` prefix is what
/// [`is_protocol_follow_up_text`](crate::services::is_protocol_follow_up_text)
/// matches on, which keeps it out of the transcript like every other
/// injected nudge.
pub const EMPTY_COMPLETION_FOLLOW_UP: &str = "Agent protocol follow-up: your last response was empty: no text and no tool call. \
     If you meant to call a tool, make the call now as a proper tool call, not inside your \
     reasoning. Otherwise give your final answer now.";

/// The start of the feedback appended after the first turn of a run the
/// provider cut off at its output cap; [`output_cap_follow_up`] is the
/// whole of it.
pub const OUTPUT_CAP_FOLLOW_UP: &str =
    "Agent protocol follow-up: your last response was cut off at the output limit";

/// The feedback for a turn cut off at `max_tokens` (the cap the request
/// carried, `None` when it carried none and the provider's own applied).
pub fn output_cap_follow_up(max_tokens: Option<u64>) -> String {
    let limit = match max_tokens {
        Some(n) => format!(" of {n} tokens"),
        None => String::new(),
    };
    format!(
        "{OUTPUT_CAP_FOLLOW_UP}{limit} before it finished; the limit counts your reasoning too. \
         Finish now, briefly: continue from where your text stopped without repeating it, or, \
         if you wrote no text yet, think less and answer directly. If you were writing a long \
         file or document, write it with tool calls in smaller parts instead."
    )
}

/// Retries the first empty tool-free model turn of a run once, and the
/// first tool-free turn cut off at the output cap once.
#[derive(Debug, Default, Clone, Copy)]
pub struct EmptyTurnRetry;

/// Run-scoped marker: the nudge has been spent on this run.
#[derive(Clone, Copy, Default)]
struct Nudged;

/// Run-scoped marker: the output-cap retry has been spent on this run.
#[derive(Clone, Copy, Default)]
struct CutShort;

/// Whether `content` carries a tool call. rig refuses to retry such a turn
/// (its calls would go unanswered), so the cap retry leaves it alone.
fn has_tool_call(content: &[AssistantContent]) -> bool {
    content
        .iter()
        .any(|item| matches!(item, AssistantContent::ToolCall(_)))
}

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
        if event.finish_reason == Some(&FinishReason::Length) && !has_tool_call(event.content) {
            if ctx.scratchpad().contains::<CutShort>() {
                tracing::warn!(
                    turn = event.turn,
                    max_tokens = ?event.max_tokens,
                    "Model turn was cut off at the output cap again; letting the run end"
                );
                return ModelTurnAction::continue_run();
            }
            tracing::warn!(
                turn = event.turn,
                max_tokens = ?event.max_tokens,
                "Model turn was cut off at the output cap; retrying once with feedback"
            );
            ctx.scratchpad().insert(CutShort);
            return ModelTurnAction::retry_with_feedback(output_cap_follow_up(event.max_tokens));
        }
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
        for cap in [Some(8_192), None] {
            let follow_up = output_cap_follow_up(cap);
            assert!(follow_up.starts_with(OUTPUT_CAP_FOLLOW_UP));
            assert!(crate::services::is_protocol_follow_up_text(&follow_up));
        }
        assert!(output_cap_follow_up(Some(8_192)).contains("of 8192 tokens"));
    }
}
