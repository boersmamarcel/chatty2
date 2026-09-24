//! The model's view of its tool-turn budget, and a graceful end to it.
//!
//! `max_agent_turns` caps rig's inner tool loop: once a `stream_prompt` run
//! has made that many model calls, rig fails the next one with
//! `MaxTurnsError` and whatever the model was about to say is lost. The
//! model is never told the budget exists: on 163 benchmark trials with a
//! local 27B model, 37 died that way, some one sentence short of the answer
//! ("…→ 4 sleighs. Let me fix:"), four SWE-bench runs without a single edit.
//!
//! [`TurnBudget`] is a per-request rig hook that fixes both halves:
//!
//! - **Awareness.** Once three quarters of the tool turns are spent, the
//!   first tool result of each turn carries a short note with how many are
//!   left ([`budget_note`]).
//! - **Graceful exhaustion.** The run gets one model call beyond the tool
//!   budget ([`TurnBudget::rig_max_turns`]). That wrap-up call advertises no
//!   tools, so the model answers in text with what it has and the run ends
//!   normally instead of erroring. All `max_agent_turns` tool turns stay
//!   usable; the extra call is the only cost.
//!
//! A headless run sends follow-up passes (finalization, stall resume,
//! loop-guard pivots) as new `stream_prompt` calls on the same history.
//! Each would otherwise get a fresh full budget, so one run made 101 tool
//! calls under `--max-agent-turns 50`. [`TurnBudget::run_share`] gives such
//! a pass only its share of the run's budget, and its notes count down the
//! run, not the pass.
//!
//! Nothing here touches history, so tool-call/result pairing (AGE-513) and
//! the context shaper's history patch (AGE-504) are unaffected: the note is
//! appended inside the tool result it rides on.

use rig_agent::agent::{
    AgentHook, CompletionCallAction, CompletionCallEvent, HookContext, RequestPatch,
    StreamingPromptRequest, ToolResultAction, ToolResultEvent,
};
use rig_core::completion::message::ToolResultContent;
use rig_core::tool::ToolOutput;

/// The note on the tool results the wrap-up call answers.
pub const FINAL_TURN_NOTE: &str = "[No tool turns left. Tools are now disabled: reply with your \
     final answer, or a short summary of what you did and what remains, using what you have.]";

/// Tool budget for one `stream_prompt` run; see the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TurnBudget {
    tool_turns: usize,
    /// Set when this `stream_prompt` run is one pass of a longer run
    /// (headless follow-ups): the run's whole budget and what earlier
    /// passes spent of it.
    run: Option<RunShare>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RunShare {
    total: usize,
    spent: usize,
}

/// Run-scoped marker: the model call whose tool results already carry a note.
#[derive(Clone, Copy)]
struct NotedTurn(usize);

impl TurnBudget {
    pub fn new(tool_turns: usize) -> Self {
        Self {
            tool_turns,
            run: None,
        }
    }

    /// One pass of a run with `total` tool turns of which earlier passes
    /// spent `spent`: this pass may use `tool_turns`, and its notes count
    /// down from the run's `total`. A pass with no tool turns left still
    /// gets the tool-free wrap-up call, so it can answer in text.
    pub fn run_share(tool_turns: usize, total: usize, spent: usize) -> Self {
        Self {
            tool_turns,
            run: Some(RunShare { total, spent }),
        }
    }

    /// Tool turns this pass may use.
    pub fn tool_turns(&self) -> usize {
        self.tool_turns
    }

    /// Set rig's call budget on `request` and register this hook on it.
    pub fn apply(self, request: StreamingPromptRequest) -> StreamingPromptRequest {
        request.max_turns(self.rig_max_turns()).add_hook(self)
    }

    /// rig's model-call budget: every tool turn plus the wrap-up call. A zero
    /// budget stays zero (rig then refuses the run outright, as before).
    pub fn rig_max_turns(&self) -> usize {
        if self.tool_turns == 0 && self.run.is_none() {
            0
        } else {
            self.tool_turns + 1
        }
    }

    /// Whether model call `turn` (one-based) is the tool-free wrap-up call.
    fn is_wrap_up(&self, turn: usize) -> bool {
        (self.tool_turns > 0 || self.run.is_some()) && turn > self.tool_turns
    }

    /// The note for model call `turn` (one-based) of this pass.
    fn note(&self, turn: usize) -> Option<String> {
        match self.run {
            None => budget_note(turn, self.tool_turns),
            Some(run) => run_budget_note(turn, self.tool_turns, run.total, run.spent),
        }
    }
}

/// The note appended to the tool results of model call `turn` (one-based)
/// under a budget of `tool_turns`, if any: nothing before three quarters of
/// the budget is spent, then the count of tool turns left, then
/// [`FINAL_TURN_NOTE`] once the next call is the wrap-up.
pub fn budget_note(turn: usize, tool_turns: usize) -> Option<String> {
    run_budget_note(turn, tool_turns, tool_turns, 0)
}

/// [`budget_note`] for model call `turn` of a pass with `pass_turns` tool
/// turns, in a run of `total` tool turns of which `spent` went to earlier
/// passes: the threshold and the "of N" are the run's.
pub fn run_budget_note(
    turn: usize,
    pass_turns: usize,
    total: usize,
    spent: usize,
) -> Option<String> {
    if pass_turns == 0 || total == 0 || (spent + turn) * 4 < total * 3 {
        return None;
    }
    if turn >= pass_turns {
        return Some(FINAL_TURN_NOTE.to_string());
    }
    let left = pass_turns - turn;
    Some(format!(
        "[{left} of {total} tool turns left. Start wrapping up: finish the current change \
         and give your answer.]"
    ))
}

/// `output` with `note` after it. Text and JSON results become one text
/// block; a multimodal result keeps its blocks and gains a text block.
fn with_note(output: &ToolOutput, note: &str) -> ToolOutput {
    if let Some(text) = output.as_text() {
        return ToolOutput::text(format!("{text}\n\n{note}"));
    }
    if let Some(value) = output.as_json() {
        return ToolOutput::text(format!("{value}\n\n{note}"));
    }
    let mut content = output.as_content().to_vec();
    content.push(ToolResultContent::text(note));
    ToolOutput::content(content).unwrap_or_else(|_| ToolOutput::text(note))
}

impl AgentHook for TurnBudget {
    async fn on_completion_call(
        &self,
        _ctx: &HookContext,
        event: CompletionCallEvent<'_>,
    ) -> CompletionCallAction {
        if self.is_wrap_up(event.turn) {
            tracing::info!(
                tool_turns = self.tool_turns,
                "Tool-turn budget spent; asking for a final answer with tools disabled"
            );
            return CompletionCallAction::patch(
                RequestPatch::new().active_tools(Vec::<String>::new()),
            );
        }
        CompletionCallAction::Continue
    }

    async fn on_tool_result(
        &self,
        ctx: &HookContext,
        event: ToolResultEvent<'_>,
    ) -> ToolResultAction {
        let turn = ctx.turn();
        let Some(note) = self.note(turn) else {
            return ToolResultAction::Keep;
        };
        // Once per turn: a parallel batch needs one note, not one per result.
        if matches!(ctx.scratchpad().get::<NotedTurn>(), Some(NotedTurn(t)) if t == turn) {
            return ToolResultAction::Keep;
        }
        ctx.scratchpad().insert(NotedTurn(turn));
        ToolResultAction::Rewrite(with_note(event.presentation, &note))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use rig_agent::agent::{AgentBuilder, MultiTurnStreamItem};
    use rig_agent::streaming::StreamingPrompt;
    use rig_agent::test_utils::MockAddTool;
    use rig_core::completion::Message;
    use rig_core::message::UserContent;
    use rig_core::test_utils::{MockCompletionModel, MockStreamEvent};

    #[test]
    fn no_note_before_three_quarters_of_the_budget() {
        assert_eq!(budget_note(7, 10), None);
        assert_eq!(budget_note(22, 30), None);
        assert_eq!(budget_note(1, 0), None);
    }

    #[test]
    fn counts_down_the_last_quarter_then_announces_the_wrap_up() {
        assert_eq!(
            budget_note(8, 10).as_deref(),
            Some(
                "[2 of 10 tool turns left. Start wrapping up: finish the current change and \
                 give your answer.]"
            )
        );
        assert!(
            budget_note(23, 30)
                .unwrap()
                .starts_with("[7 of 30 tool turns left.")
        );
        assert!(
            budget_note(29, 30)
                .unwrap()
                .starts_with("[1 of 30 tool turns left.")
        );
        assert_eq!(budget_note(30, 30).as_deref(), Some(FINAL_TURN_NOTE));
        // A one-turn budget goes straight to the wrap-up note.
        assert_eq!(budget_note(1, 1).as_deref(), Some(FINAL_TURN_NOTE));
    }

    #[test]
    fn rig_budget_is_one_call_past_the_tool_budget() {
        assert_eq!(TurnBudget::new(30).rig_max_turns(), 31);
        assert_eq!(TurnBudget::new(0).rig_max_turns(), 0);
        let budget = TurnBudget::new(2);
        assert!(!budget.is_wrap_up(2));
        assert!(budget.is_wrap_up(3));
    }

    #[test]
    fn a_run_share_counts_down_the_run() {
        // Pass 2 of a 50-turn run that already spent 45: its 5 turns are
        // the run's last, and the note says so from its first call.
        assert!(
            run_budget_note(1, 5, 50, 45)
                .unwrap()
                .starts_with("[4 of 50 tool turns left.")
        );
        assert_eq!(
            run_budget_note(5, 5, 50, 45).as_deref(),
            Some(FINAL_TURN_NOTE)
        );
        // Early in the run a follow-up pass stays quiet.
        assert_eq!(run_budget_note(1, 40, 50, 10), None);
    }

    #[test]
    fn a_spent_run_share_still_gets_its_wrap_up_call() {
        let spent = TurnBudget::run_share(0, 50, 50);
        assert_eq!(spent.rig_max_turns(), 1);
        assert!(spent.is_wrap_up(1));
        assert_eq!(TurnBudget::run_share(4, 50, 48).rig_max_turns(), 5);
        // The standalone budget is unchanged.
        assert_eq!(TurnBudget::new(0).rig_max_turns(), 0);
        assert!(!TurnBudget::new(0).is_wrap_up(1));
    }

    #[test]
    fn note_follows_text_and_json_results() {
        let text = with_note(&ToolOutput::text("42"), "[note]");
        assert_eq!(text.as_text(), Some("42\n\n[note]"));
        let json = with_note(&ToolOutput::json(serde_json::json!({"a": 1})), "[note]");
        assert_eq!(json.as_text(), Some("{\"a\":1}\n\n[note]"));
    }

    fn tool_call_turn(n: u32) -> Vec<MockStreamEvent> {
        vec![
            MockStreamEvent::tool_call(
                format!("tool_call_{n}"),
                "add",
                serde_json::json!({"x": 1, "y": 2}),
            )
            .with_call_id(format!("call_{n}")),
            MockStreamEvent::final_response_with_total_tokens(4),
        ]
    }

    fn tool_result_texts(message: &Message) -> Vec<String> {
        match message {
            Message::User { content } => content
                .iter()
                .filter_map(|c| match c {
                    UserContent::ToolResult(r) => Some(
                        r.content
                            .iter()
                            .map(|b| match (b.as_text(), b.as_json()) {
                                (Some(text), _) => text.to_owned(),
                                (None, Some(value)) => value.to_string(),
                                (None, None) => String::new(),
                            })
                            .collect::<String>(),
                    ),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    /// A model that would keep calling tools past its budget gets one more
    /// call with no tools advertised, and its text ends the run normally
    /// instead of `MaxTurnsError`.
    #[tokio::test]
    async fn exhausted_budget_ends_with_a_tool_free_final_answer() {
        let model = MockCompletionModel::from_stream_turns([
            tool_call_turn(1),
            tool_call_turn(2),
            vec![
                MockStreamEvent::text("best answer so far: 3"),
                MockStreamEvent::final_response_with_total_tokens(6),
            ],
        ]);
        let agent = AgentBuilder::new(model.clone()).tool(MockAddTool).build();

        let mut stream = TurnBudget::new(2).apply(agent.stream_prompt("add")).await;
        let mut final_text = None;
        while let Some(item) = stream.next().await {
            if let MultiTurnStreamItem::FinalResponse(response) =
                item.expect("the run ends without an error")
            {
                final_text = Some(response.output);
            }
        }
        assert_eq!(final_text.as_deref(), Some("best answer so far: 3"));

        let requests = model.requests();
        assert_eq!(requests.len(), 3);
        assert!(!requests[1].tools.is_empty(), "tool turns keep their tools");
        assert!(requests[2].tools.is_empty(), "the wrap-up call has none");

        // Turn 1 of 2 is under the threshold (1*4 < 2*3); turn 2's result
        // tells the model the next reply is its last.
        let last = |i: usize| requests[i].chat_history.last().cloned().unwrap();
        assert_eq!(tool_result_texts(&last(1)), vec!["3".to_string()]);
        assert_eq!(
            tool_result_texts(&last(2)),
            vec![format!("3\n\n{FINAL_TURN_NOTE}")]
        );
    }
}
