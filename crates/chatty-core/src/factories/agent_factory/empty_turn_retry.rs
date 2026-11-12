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
//! human can see it may be incomplete. So a cap costs a run at most one
//! extra model call, never a loop of truncated answers. A cut-off turn with
//! no answer at all takes the ladder below instead.
//! # A turn the window ran out under (AGE-967)
//!
//! A tool-free turn cut off with no answer at all — on a thinking model,
//! one that spent the whole output allowance reasoning — gets a short
//! ladder instead of the single "think less" retry, cheapest step first:
//!
//! 1. **Thinking off.** When the turn thought (reasoning content or
//!    reasoning tokens) and the agent knows its provider's switch
//!    ([`EmptyTurnRetry::with_thinking_off`]: vLLM's
//!    `chat_template_kwargs.enable_thinking`, Ollama's `think`), the turn is
//!    repeated with that switch in the request — for this call and every
//!    later one of the run. Repeated, not answered with feedback: the
//!    rejected reasoning stays out of the history, so the retry is the
//!    same request, prompt cache and all. Otherwise the AGE-936 feedback
//!    retry takes this step.
//! 2. **Room for the reply.** When the cut-off was the model's window, not
//!    a cap chatty chose (the request carried no `max_tokens`, or the
//!    context guard clamped it to the window), the turn is repeated once
//!    more with the context guard told to keep half the window for the
//!    reply ([`ReplyRoom`]): it shapes — and in an unattended run compacts
//!    — the history that much harder, with the results ledger keeping the
//!    delegations it drops. Skipped when the guard already keeps that much
//!    (an output cap of half the window or more), since the repeat would be
//!    the identical request. Otherwise, when the feedback retry is still
//!    unspent, it takes this step.
//! 3. **Continue later.** The run ends with rig's "finish_reason=Length"
//!    error, and the session handler ends the turn with a `FollowUp`
//!    instead of an `EmptyCompletion` error, once per chain
//!    ([`CUT_OFF_FOLLOW_UP`](crate::session::CUT_OFF_FOLLOW_UP)); hive
//!    continues it server-side within its follow-up limit.
//!
//! So a run pays at most two extra model calls for a cut-off, and the
//! answer a cap cut short (text, no tool call) keeps the AGE-936 behaviour.
//!
//! [`StreamErrorKind::EmptyCompletion`]: crate::services::StreamErrorKind::EmptyCompletion

use rig_agent::agent::{
    AgentHook, CompletionCallAction, CompletionCallEvent, HookContext, ModelTurnAction,
    ModelTurnFinished, RequestPatch,
};
use rig_core::completion::FinishReason;
use rig_core::message::AssistantContent;

use crate::services::context_shaper::{ReplyRoom, WindowClamped};

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

/// Retries the first empty tool-free model turn of a run once, the first
/// tool-free turn cut off at the output cap once, and a turn cut off with
/// no answer along the ladder in the module docs.
#[derive(Debug, Default, Clone)]
pub struct EmptyTurnRetry {
    /// The request fields that switch this agent's thinking off, when its
    /// provider has a known switch.
    thinking_off: Option<serde_json::Value>,
    /// Whether the context guard has more room to keep for the reply than
    /// it already does ([`ContextShaper::reply_room_reserve`] above zero).
    /// Without it the reply-room step would repeat the identical request.
    ///
    /// [`ContextShaper::reply_room_reserve`]: crate::services::context_shaper::ContextShaper::reply_room_reserve
    reply_room: bool,
}

impl EmptyTurnRetry {
    /// The hook for an agent whose requests switch thinking off with
    /// `params` (merged into the request's provider fields; `None` when the
    /// provider has no known switch).
    pub fn with_thinking_off(params: Option<serde_json::Value>) -> Self {
        Self {
            thinking_off: params,
            reply_room: false,
        }
    }

    /// Whether the reply-room step can change the request: the agent's
    /// context guard reserves less than [`REPLY_ROOM_SHARE`] of the window
    /// for the reply today. When it already reserves that much (an output
    /// cap of half the window or more), the step is skipped for the next
    /// one.
    ///
    /// [`REPLY_ROOM_SHARE`]: crate::services::context_shaper::REPLY_ROOM_SHARE
    pub fn with_reply_room(mut self, reply_room: bool) -> Self {
        self.reply_room = reply_room;
        self
    }
}

/// Run-scoped marker: the nudge has been spent on this run.
#[derive(Clone, Copy, Default)]
struct Nudged;

/// Run-scoped marker: the output-cap feedback retry has been spent on this
/// run.
#[derive(Clone, Copy, Default)]
struct CutShort;

/// Run-scoped marker: a turn ran out with no answer, and every later model
/// call of the run goes out with thinking off.
#[derive(Clone, Copy, Default)]
struct ThinkingOff;

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
        _ => false,
    })
}

/// Whether the turn reasoned: reasoning content, or reasoning tokens the
/// provider counted without surfacing them.
fn thought(content: &[AssistantContent], reasoning_tokens: u64) -> bool {
    reasoning_tokens > 0
        || content
            .iter()
            .any(|item| matches!(item, AssistantContent::Reasoning(_)))
}

/// The next step of the ladder (module docs) for a tool-free turn cut off
/// with no answer, or `None` when it is spent and the run should end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CutOffStep {
    /// Repeat the turn with thinking off.
    ThinkingOff,
    /// Repeat the turn with the context guard keeping room for the reply.
    ReplyRoom,
    /// The AGE-936 feedback retry.
    Feedback,
}

/// What the run has already spent on cut-offs, read off its scratchpad.
#[derive(Debug, Clone, Copy, Default)]
struct Spent {
    thinking_off: bool,
    reply_room: bool,
    feedback: bool,
}

/// The ladder: thinking off first when the turn thought and the switch is
/// known, then room for the reply when the window was what cut it off,
/// with the feedback retry standing in for a step that does not apply.
/// Two steps at most.
fn next_cut_off_step(
    spent: Spent,
    thought: bool,
    has_switch: bool,
    window_bound: bool,
) -> Option<CutOffStep> {
    let steps_taken = [spent.thinking_off, spent.reply_room, spent.feedback]
        .iter()
        .filter(|taken| **taken)
        .count();
    if steps_taken >= 2 {
        return None;
    }
    if thought && has_switch && !spent.thinking_off {
        return Some(CutOffStep::ThinkingOff);
    }
    if window_bound && !spent.reply_room {
        return Some(CutOffStep::ReplyRoom);
    }
    if !spent.feedback {
        return Some(CutOffStep::Feedback);
    }
    None
}

impl AgentHook for EmptyTurnRetry {
    async fn on_completion_call(
        &self,
        ctx: &HookContext,
        _event: CompletionCallEvent<'_>,
    ) -> CompletionCallAction {
        match &self.thinking_off {
            Some(params) if ctx.scratchpad().contains::<ThinkingOff>() => {
                CompletionCallAction::patch(RequestPatch::new().additional_params(params.clone()))
            }
            _ => CompletionCallAction::Continue,
        }
    }

    async fn on_model_turn_finished(
        &self,
        ctx: &HookContext,
        event: ModelTurnFinished<'_>,
    ) -> ModelTurnAction {
        let cut_off =
            event.finish_reason == Some(&FinishReason::Length) && !has_tool_call(event.content);
        if cut_off && !has_output(event.content) {
            return self.on_cut_off_without_answer(ctx, &event);
        }
        if cut_off {
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

impl EmptyTurnRetry {
    /// A tool-free turn cut off before it answered anything: the next step
    /// of the ladder in the module docs, or the end of the run.
    fn on_cut_off_without_answer(
        &self,
        ctx: &HookContext,
        event: &ModelTurnFinished<'_>,
    ) -> ModelTurnAction {
        let pad = ctx.scratchpad();
        let spent = Spent {
            thinking_off: pad.contains::<ThinkingOff>(),
            reply_room: pad.contains::<ReplyRoom>(),
            feedback: pad.contains::<CutShort>(),
        };
        // No cap in the request means the provider's own, which on vLLM is
        // whatever the window has left; a cap the guard clamped is the window
        // too. Either way a shorter history is more room for the reply.
        // And the step only helps when it changes the request: with the
        // reply already given half the window, it would repeat it unchanged.
        let window_bound =
            (event.max_tokens.is_none() || pad.contains::<WindowClamped>()) && self.reply_room;
        let step = next_cut_off_step(
            spent,
            thought(event.content, event.usage.reasoning_tokens.unwrap_or(0)),
            self.thinking_off.is_some(),
            window_bound,
        );
        let Some(step) = step else {
            tracing::warn!(
                turn = event.turn,
                max_tokens = ?event.max_tokens,
                "Model turn was cut off with no answer again; ending the run for a follow-up"
            );
            return ModelTurnAction::continue_run();
        };
        tracing::warn!(
            turn = event.turn,
            max_tokens = ?event.max_tokens,
            output_tokens = event.usage.output_tokens,
            reasoning_tokens = event.usage.reasoning_tokens,
            ?step,
            "Model turn was cut off at the output cap with no answer; retrying"
        );
        match step {
            CutOffStep::ThinkingOff => {
                pad.insert(ThinkingOff);
                ModelTurnAction::repeat()
            }
            CutOffStep::ReplyRoom => {
                pad.insert(ReplyRoom);
                ModelTurnAction::repeat()
            }
            CutOffStep::Feedback => {
                pad.insert(CutShort);
                ModelTurnAction::retry_with_feedback(output_cap_follow_up(event.max_tokens))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig_core::message::{CallId, Reasoning, ToolCall, ToolFunction};

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
            CallId::from_wire("call-1".to_string()),
            ToolFunction::new(
                crate::testing::tool_name("read_file"),
                serde_json::json!({"path": "x"})
            ),
        ))]));
    }

    /// AGE-967: the cut-off ladder — thinking off first when the turn
    /// thought and the switch is known, then room for the reply when the
    /// window was the limit, with the feedback retry standing in for a step
    /// that does not apply — and never more than two steps.
    #[test]
    fn the_cut_off_ladder() {
        use CutOffStep::*;
        let none = Spent::default();
        let off = Spent {
            thinking_off: true,
            ..none
        };
        // The soak: a thinking model on vLLM, no cap in the request.
        assert_eq!(next_cut_off_step(none, true, true, true), Some(ThinkingOff));
        assert_eq!(next_cut_off_step(off, true, true, true), Some(ReplyRoom));
        let both = Spent {
            reply_room: true,
            ..off
        };
        assert_eq!(next_cut_off_step(both, true, true, true), None);
        // A cap chatty chose: a shorter history buys nothing.
        assert_eq!(next_cut_off_step(off, true, true, false), Some(Feedback));
        // No switch (or no reasoning to switch off): feedback first.
        assert_eq!(next_cut_off_step(none, true, false, true), Some(ReplyRoom));
        assert_eq!(next_cut_off_step(none, false, true, false), Some(Feedback));
        let fed = Spent {
            feedback: true,
            ..none
        };
        assert_eq!(next_cut_off_step(fed, false, true, false), None);
        assert_eq!(next_cut_off_step(fed, false, true, true), Some(ReplyRoom));
    }

    #[test]
    fn reasoning_content_or_tokens_is_thought() {
        assert!(!thought(&[], 0));
        assert!(thought(&[], 12));
        assert!(thought(
            &[AssistantContent::Reasoning(Reasoning::new("hmm"))],
            0
        ));
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
