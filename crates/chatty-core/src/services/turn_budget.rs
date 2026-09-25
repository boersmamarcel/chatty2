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
//! A budget of `0` tool turns means no cap (the interactive default: the
//! user has Stop). rig still needs a number, so such a run gets
//! [`UNCAPPED_RIG_TURNS`], no notes and no wrap-up call.
//!
//! A run without a human (headless) can carry a wall-clock [`Deadline`]
//! instead of, or next to, a turn cap: from 85 % of it on, tool results say
//! how much time is left, and the first model call after it is the same
//! tool-free wrap-up call.
//!
//! Should the wrap-up call ask for a tool anyway (an unparsed call left in
//! the reasoning channel trips `EmptyTurnRetry` into one call too many; a
//! parsed one is a disallowed tool), the run still ends with the model's
//! text: `llm_service` turns the resulting `MaxTurnsError` / the stop this
//! hook asks for ([`WRAP_UP_TOOL_CALL_STOP`]) into a normal end.
//!
//! Nothing here touches history, so tool-call/result pairing (AGE-513) and
//! the context shaper's history patch (AGE-504) are unaffected: the note is
//! appended inside the tool result it rides on.

use std::time::{Duration, Instant};

use rig_agent::agent::{
    AgentHook, CompletionCallAction, CompletionCallEvent, HookContext, InvalidToolCallAction,
    InvalidToolCallContext, RequestPatch, StreamingPromptRequest, ToolResultAction,
    ToolResultEvent,
};
use rig_core::completion::message::ToolResultContent;
use rig_core::tool::ToolOutput;

/// The note on the tool results the wrap-up call answers.
pub const FINAL_TURN_NOTE: &str = "[No tool turns left. Tools are now disabled: reply with your \
     final answer, or a short summary of what you did and what remains, using what you have.]";

/// The note on the tool results the wrap-up call answers once the run's
/// [`Deadline`] has passed.
pub const TIME_UP_NOTE: &str = "[Time is up. Tools are now disabled: reply with your final \
     answer, or a short summary of what you did and what remains, using what you have.]";

/// What the countdown notes ask for. A run that has only explored so far
/// must hear that it is time to act, not merely to wrap up.
const FINISH_NOW: &str = "If you have not yet applied your fix or written your answer, do it \
     now with your best current solution; then reply with your final answer.";

/// rig's model-call budget for a run with no turn cap.
pub const UNCAPPED_RIG_TURNS: usize = 1_000_000;

/// The share of a [`Deadline`] after which tool results carry a time note.
const TIME_NOTE_PERCENT: u32 = 85;

/// The reason this hook stops a run whose tool-free wrap-up call asked for
/// a tool; `llm_service` ends such a run normally instead of as an error.
pub const WRAP_UP_TOOL_CALL_STOP: &str = "turn budget: the tool-free wrap-up call asked for a tool";

/// A run's wall-clock budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Deadline {
    start: Instant,
    end: Instant,
}

impl Deadline {
    /// A budget of `budget` starting at `start`.
    pub fn new(start: Instant, budget: Duration) -> Self {
        Self {
            start,
            end: start + budget,
        }
    }

    /// A budget of `budget` starting now.
    pub fn starting_now(budget: Duration) -> Self {
        Self::new(Instant::now(), budget)
    }

    /// When the budget runs out.
    pub fn end(&self) -> Instant {
        self.end
    }

    /// The whole budget.
    pub fn budget(&self) -> Duration {
        self.end - self.start
    }

    /// Whether the budget has run out at `now`.
    pub fn is_past(&self, now: Instant) -> bool {
        now >= self.end
    }

    /// The time note for `now`, if one is due: nothing before
    /// [`TIME_NOTE_PERCENT`] of the budget, then the time left, then
    /// [`TIME_UP_NOTE`].
    pub fn note(&self, now: Instant) -> Option<String> {
        if self.is_past(now) {
            return Some(TIME_UP_NOTE.to_string());
        }
        if now < self.start + self.budget() * TIME_NOTE_PERCENT / 100 {
            return None;
        }
        let left = self.end - now;
        let left = if left < Duration::from_secs(60) {
            "Less than a minute".to_string()
        } else {
            format!("About {} min", left.as_secs().div_ceil(60))
        };
        Some(format!(
            "[{left} left in this run's time budget. {FINISH_NOW}]"
        ))
    }
}

/// Tool budget for one `stream_prompt` run; see the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TurnBudget {
    tool_turns: usize,
    /// Set when this `stream_prompt` run is one pass of a longer run
    /// (headless follow-ups): the run's whole budget and what earlier
    /// passes spent of it.
    run: Option<RunShare>,
    /// The run's wall-clock budget, when it has one.
    deadline: Option<Deadline>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RunShare {
    total: usize,
    spent: usize,
}

/// Run-scoped marker: the model call whose tool results already carry a note.
#[derive(Clone, Copy)]
struct NotedTurn(usize);

/// Run-scoped marker: the model call that went out with tools disabled.
#[derive(Clone, Copy)]
struct WrapUpCall(usize);

impl TurnBudget {
    /// A budget of `tool_turns` tool turns; `0` is no cap.
    pub fn new(tool_turns: usize) -> Self {
        Self {
            tool_turns,
            run: None,
            deadline: None,
        }
    }

    /// This budget with the run's wall-clock `deadline`, if it has one.
    pub fn with_deadline(self, deadline: Option<Deadline>) -> Self {
        Self { deadline, ..self }
    }

    /// One pass of a run with `total` tool turns of which earlier passes
    /// spent `spent`: this pass may use `tool_turns`, and its notes count
    /// down from the run's `total`. A pass with no tool turns left still
    /// gets the tool-free wrap-up call, so it can answer in text.
    pub fn run_share(tool_turns: usize, total: usize, spent: usize) -> Self {
        Self {
            tool_turns,
            run: Some(RunShare { total, spent }),
            deadline: None,
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

    /// Whether this pass has a turn cap: a standalone budget of `0` has none.
    fn is_capped(&self) -> bool {
        self.tool_turns > 0 || self.run.is_some()
    }

    /// rig's model-call budget: every tool turn plus the wrap-up call, or
    /// [`UNCAPPED_RIG_TURNS`] without a cap.
    pub fn rig_max_turns(&self) -> usize {
        if self.is_capped() {
            self.tool_turns + 1
        } else {
            UNCAPPED_RIG_TURNS
        }
    }

    /// Whether model call `turn` (one-based), made at `now`, is the
    /// tool-free wrap-up call: the tool turns are spent, or the deadline
    /// has passed.
    fn is_wrap_up(&self, turn: usize, now: Instant) -> bool {
        (self.is_capped() && turn > self.tool_turns)
            || self.deadline.is_some_and(|d| d.is_past(now))
    }

    /// The note for the tool results of model call `turn` (one-based) of
    /// this pass at `now`: the turn countdown, the time countdown, or both.
    fn note(&self, turn: usize, now: Instant) -> Option<String> {
        let turns = match self.run {
            None => budget_note(turn, self.tool_turns),
            Some(run) => run_budget_note(turn, self.tool_turns, run.total, run.spent),
        };
        let time = self.deadline.and_then(|d| d.note(now));
        match (turns, time) {
            // Whichever says tools are gone is the one that matters.
            (Some(turns), _) if turns == FINAL_TURN_NOTE => Some(turns),
            (_, Some(time)) if time == TIME_UP_NOTE => Some(time),
            (Some(turns), Some(time)) => Some(format!("{turns}\n{time}")),
            (turns, time) => turns.or(time),
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
    Some(format!("[{left} of {total} tool turns left. {FINISH_NOW}]"))
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
        ctx: &HookContext,
        event: CompletionCallEvent<'_>,
    ) -> CompletionCallAction {
        if self.is_wrap_up(event.turn, Instant::now()) {
            tracing::info!(
                tool_turns = self.tool_turns,
                turn = event.turn,
                "Turn or time budget spent; asking for a final answer with tools disabled"
            );
            ctx.scratchpad().insert(WrapUpCall(event.turn));
            return CompletionCallAction::patch(
                RequestPatch::new().active_tools(Vec::<String>::new()),
            );
        }
        CompletionCallAction::Continue
    }

    /// A tool call from the wrap-up call has nowhere to go: another model
    /// call would only exceed rig's budget. Stop the run; `llm_service`
    /// ends it with whatever text the model gave.
    async fn on_invalid_tool_call(
        &self,
        ctx: &HookContext,
        event: &InvalidToolCallContext,
    ) -> Option<InvalidToolCallAction> {
        let wrap_up = matches!(
            ctx.scratchpad().get::<WrapUpCall>(),
            Some(WrapUpCall(t)) if t == ctx.turn()
        );
        if !wrap_up {
            return None;
        }
        tracing::warn!(
            tool = %event.tool_name,
            "The tool-free wrap-up call asked for a tool; ending the run with its text"
        );
        Some(InvalidToolCallAction::stop(WRAP_UP_TOOL_CALL_STOP))
    }

    async fn on_tool_result(
        &self,
        ctx: &HookContext,
        event: ToolResultEvent<'_>,
    ) -> ToolResultAction {
        let turn = ctx.turn();
        let Some(note) = self.note(turn, Instant::now()) else {
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
                "[2 of 10 tool turns left. If you have not yet applied your fix or written your \
                 answer, do it now with your best current solution; then reply with your final \
                 answer.]"
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
        let now = Instant::now();
        assert_eq!(TurnBudget::new(30).rig_max_turns(), 31);
        let budget = TurnBudget::new(2);
        assert!(!budget.is_wrap_up(2, now));
        assert!(budget.is_wrap_up(3, now));
    }

    /// `0` is no cap: rig gets a number nothing reaches, and the run never
    /// hears about turns.
    #[test]
    fn a_zero_budget_is_uncapped() {
        let now = Instant::now();
        let uncapped = TurnBudget::new(0);
        assert_eq!(uncapped.rig_max_turns(), UNCAPPED_RIG_TURNS);
        assert!(!uncapped.is_wrap_up(10_000, now));
        assert_eq!(uncapped.note(10_000, now), None);
    }

    #[test]
    fn the_time_note_starts_at_85_percent_and_counts_down() {
        let start = Instant::now();
        let deadline = Deadline::new(start, Duration::from_secs(1000));
        assert_eq!(deadline.note(start + Duration::from_secs(849)), None);
        assert_eq!(
            deadline.note(start + Duration::from_secs(850)).as_deref(),
            Some(
                "[About 3 min left in this run's time budget. If you have not yet applied your \
                 fix or written your answer, do it now with your best current solution; then \
                 reply with your final answer.]"
            )
        );
        assert!(
            deadline
                .note(start + Duration::from_secs(990))
                .unwrap()
                .starts_with("[Less than a minute left")
        );
        assert_eq!(
            deadline.note(start + Duration::from_secs(1000)).as_deref(),
            Some(TIME_UP_NOTE)
        );
    }

    /// Past the deadline the next call is the wrap-up, whatever turn it is
    /// and whether or not the run has a turn cap.
    #[test]
    fn a_passed_deadline_makes_the_next_call_the_wrap_up() {
        let start = Instant::now();
        let deadline = Some(Deadline::new(start, Duration::from_secs(60)));
        let before = start + Duration::from_secs(59);
        let after = start + Duration::from_secs(60);
        for budget in [TurnBudget::new(0), TurnBudget::new(50)] {
            let budget = budget.with_deadline(deadline);
            assert!(!budget.is_wrap_up(1, before));
            assert!(budget.is_wrap_up(1, after));
            assert_eq!(budget.note(1, after).as_deref(), Some(TIME_UP_NOTE));
        }
    }

    #[test]
    fn turn_and_time_notes_combine_and_the_final_one_wins() {
        let start = Instant::now();
        let late = start + Duration::from_secs(90);
        let budget =
            TurnBudget::new(10).with_deadline(Some(Deadline::new(start, Duration::from_secs(100))));
        let both = budget.note(8, late).unwrap();
        assert!(both.starts_with("[2 of 10 tool turns left."), "{both}");
        assert!(both.contains("\n[Less than a minute left"), "{both}");
        assert_eq!(budget.note(10, late).as_deref(), Some(FINAL_TURN_NOTE));
        assert_eq!(
            budget.note(3, late + Duration::from_secs(10)).as_deref(),
            Some(TIME_UP_NOTE)
        );
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
        let now = Instant::now();
        let spent = TurnBudget::run_share(0, 50, 50);
        assert_eq!(spent.rig_max_turns(), 1);
        assert!(spent.is_wrap_up(1, now));
        assert_eq!(TurnBudget::run_share(4, 50, 48).rig_max_turns(), 5);
        // A standalone zero budget is no cap, not a spent one.
        assert_eq!(TurnBudget::new(0).rig_max_turns(), UNCAPPED_RIG_TURNS);
        assert!(!TurnBudget::new(0).is_wrap_up(1, now));
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

    /// An uncapped run past its deadline: the first call already goes out
    /// without tools, and its text ends the run.
    #[tokio::test]
    async fn a_passed_deadline_ends_an_uncapped_run_with_a_tool_free_answer() {
        let model = MockCompletionModel::from_stream_turns([vec![
            MockStreamEvent::text("out of time: 3"),
            MockStreamEvent::final_response_with_total_tokens(6),
        ]]);
        let agent = AgentBuilder::new(model.clone()).tool(MockAddTool).build();
        let budget = TurnBudget::new(0).with_deadline(Some(Deadline::new(
            Instant::now() - Duration::from_secs(2),
            Duration::from_secs(1),
        )));

        let mut stream = budget.apply(agent.stream_prompt("add")).await;
        let mut final_text = None;
        while let Some(item) = stream.next().await {
            if let MultiTurnStreamItem::FinalResponse(response) =
                item.expect("the run ends without an error")
            {
                final_text = Some(response.output);
            }
        }
        assert_eq!(final_text.as_deref(), Some("out of time: 3"));
        assert!(model.requests()[0].tools.is_empty());
    }

    /// An uncapped run keeps its tools and hears nothing about turns.
    #[tokio::test]
    async fn an_uncapped_run_keeps_its_tools() {
        let mut turns: Vec<_> = (1..=12).map(tool_call_turn).collect();
        turns.push(vec![
            MockStreamEvent::text("done"),
            MockStreamEvent::final_response_with_total_tokens(6),
        ]);
        let model = MockCompletionModel::from_stream_turns(turns);
        let agent = AgentBuilder::new(model.clone()).tool(MockAddTool).build();

        let mut stream = TurnBudget::new(0).apply(agent.stream_prompt("add")).await;
        while let Some(item) = stream.next().await {
            item.expect("the run ends without an error");
        }
        let requests = model.requests();
        assert_eq!(requests.len(), 13);
        assert!(requests.iter().all(|r| !r.tools.is_empty()));
        let last = requests[12].chat_history.last().cloned().unwrap();
        assert_eq!(tool_result_texts(&last), vec!["3".to_string()]);
    }

    /// The wrap-up call answering with a tool call anyway is stopped with
    /// [`WRAP_UP_TOOL_CALL_STOP`] (which `llm_service` ends normally), not
    /// failed as an unknown tool.
    #[tokio::test]
    async fn a_tool_call_in_the_wrap_up_call_stops_with_the_budget_reason() {
        let model = MockCompletionModel::from_stream_turns([
            tool_call_turn(1),
            tool_call_turn(2),
            tool_call_turn(3),
        ]);
        let agent = AgentBuilder::new(model.clone()).tool(MockAddTool).build();

        let mut stream = TurnBudget::new(2).apply(agent.stream_prompt("add")).await;
        let mut error = None;
        while let Some(item) = stream.next().await {
            if let Err(e) = item {
                error = Some(e);
            }
        }
        let error = error.expect("rig reports the stop");
        assert!(
            matches!(
                &error,
                rig_agent::agent::StreamingError::Prompt(e)
                    if matches!(
                        e.as_ref(),
                        rig_agent::completion::PromptError::PromptCancelled { reason, .. }
                            if reason == WRAP_UP_TOOL_CALL_STOP
                    )
            ),
            "{error}"
        );
        assert_eq!(model.requests().len(), 3);
    }
}
