//! A spent budget ends the whole tree (AGE-903).
//!
//! When the model endpoint refuses a call with `402` and a body naming
//! `budget_exceeded` or `cap_exceeded` (hive's egress and turn route), or
//! the run's own spend gate refuses a delegation (`cap_exceeded`,
//! `budget_spent: usd`), nothing in the tree may spend again:
//!
//! - **The refused call is terminal.** `llm_service` classifies it as
//!   [`StreamErrorKind::BudgetExhausted`](crate::services::StreamErrorKind),
//!   which the D5 table never retries or nudges, and its message is the
//!   typed [`budget_exhausted`] text. A worker's task fails with it, its
//!   streamed text, usage and captured conversation riding the result as
//!   any failed task's do.
//! - **The caller stops.** `invoke_agent` returns it as
//!   `InvokeAgentError::BudgetExhausted`, refuses every later delegation of
//!   the run and hangs up on the siblings still running (the TB-7 path: the
//!   broker reaps a hung-up call's subtree). [`StopOnBudgetExhausted`] ends
//!   the calling model's run at its next model call — which would only be
//!   refused, or spend past the budget — with [`final_message`] as its
//!   answer: the budget line and what was finished, no model call.
//! - **It travels up.** A sub-leader's task fails with the same typed text
//!   (the worker's frame mapper finds it in the failed `invoke_agent`), so
//!   its own caller stops too, and the root shows the final message.

use rig_agent::agent::{
    AgentHook, CompletionCallAction, CompletionCallEvent, HookContext, OutcomeAction, OutcomeEvent,
};
use rig_agent::tool::Tool;

use crate::tools::best_of_tool::BestOfTool;
use crate::tools::invoke_agent_tool::InvokeAgentTool;

/// The typed reason a run stopped on a spent budget. It leads the failed
/// result's error — `budget_exhausted: <headline>` — and every caller up
/// the tree passes it on.
pub const BUDGET_EXHAUSTED: &str = "budget_exhausted";

/// What the calling model reads under a budget stop. It never gets to act
/// on it — the run stops at its next call — but a transcript shows why.
pub const BUDGET_STOP_NOTE: &str = "The budget is spent: no agent may make another model call \
     or delegation. Stop here.";

/// The reason [`StopOnBudgetExhausted`] stops a run with, ahead of the
/// final message; `llm_service` ends such a run normally with the message
/// as its text.
pub const BUDGET_STOP: &str = "budget exhausted\n";

/// The headline a failed `best_of` stops the run with (AGE-1132). It is not a
/// spent budget: [`final_message`] words it as a best_of failure.
pub const BEST_OF_FAILED: &str = "best_of: every attempt failed";

/// What the calling model reads under a failed `best_of`.
pub const BEST_OF_STOP_NOTE: &str =
    "best_of failed on every attempt: no agent may start another delegation. Stop here.";

/// `budget_exhausted: <headline>`.
pub fn budget_exhausted(headline: &str) -> String {
    format!("{BUDGET_EXHAUSTED}: {headline}")
}

/// What a stopped run's callers read as the reason: `budget_exhausted:
/// <headline>` for a spent budget, the bare headline for a failed `best_of`
/// (no budget wording, and no marker a budget-stop reader would match).
pub fn stop_text(headline: &str) -> String {
    if headline.starts_with(BEST_OF_FAILED) {
        headline.to_string()
    } else {
        budget_exhausted(headline)
    }
}

/// The line after [`stop_text`] telling the model to stop.
pub fn stop_note(headline: &str) -> &'static str {
    if headline.starts_with(BEST_OF_FAILED) {
        BEST_OF_STOP_NOTE
    } else {
        BUDGET_STOP_NOTE
    }
}

/// "Budget of $X spent", or "Budget spent" when the figure is unknown.
fn budget_line(cap_usd: Option<f64>) -> String {
    match cap_usd {
        Some(cap) => format!("Budget of ${cap:.2} spent"),
        None => "Budget spent".to_string(),
    }
}

/// The typed error text for a model endpoint's refusal, when `status` and
/// `body` are one: a `402` whose JSON body names `budget_exceeded` or
/// `cap_exceeded` as its `error` (hive's `{"error": "cap_exceeded",
/// "month_to_date": …, "cap_usd": …}`), its `code`, or its nested
/// `error.code` (OpenAI's envelope). The dollar figure is the body's
/// `cap_usd` or `budget_usd`; the provider's `message` (top-level or
/// `error.message`) is kept after it. `None` for any other response.
pub fn endpoint_refusal(status: u16, body: &str) -> Option<String> {
    if status != 402 {
        return None;
    }
    let json: serde_json::Value = serde_json::from_str(body).ok()?;
    let code = json["error"]
        .as_str()
        .or_else(|| json["code"].as_str())
        .or_else(|| json["error"]["code"].as_str())?;
    if !matches!(code, "budget_exceeded" | "cap_exceeded") {
        return None;
    }
    let cap = json["cap_usd"]
        .as_f64()
        .or_else(|| json["budget_usd"].as_f64());
    let message = json["message"]
        .as_str()
        .or_else(|| json["error"]["message"].as_str())
        .map(|m| format!(": {}", m.trim()))
        .unwrap_or_default();
    Some(budget_exhausted(&format!(
        "{}; the model endpoint refused the call ({code}){message}",
        budget_line(cap)
    )))
}

/// The one fixed wrapper a typed stop is read through: how
/// `map_tool_error` hands a failed `invoke_agent` to the model, the stream
/// and the worker's frame mapper.
const TOOL_ERROR_PREFIX: &str = "Error: invoke_agent: ";

/// The headline of the budget stop `text` *leads with* — itself, or after
/// the tool-error wrapper `Error: invoke_agent: `: a [`budget_exhausted`]
/// text (up to its first blank line), or a spend gate's refusal of a
/// delegation (`cap_exceeded: … cap $Y …`, `budget_spent: usd`), which is
/// the same stop when the gate is local (DP-3).
///
/// Only a leading typed text counts. Every typed stop leads its error
/// (`InvokeAgentError::BudgetExhausted`, `CapExceeded` and the
/// `budget_spent` refusal display that way, and so does a worker's failed
/// task); the same words further in — a generic failure's
/// `Agent '…' reported failure: …`, tree messages delivered after an
/// error, a remote agent's own text — are someone quoting them, and must
/// not stop the tree.
pub fn find_budget_exhausted(text: &str) -> Option<String> {
    let text = text.trim_start();
    let text = text.strip_prefix(TOOL_ERROR_PREFIX).unwrap_or(text);
    let first_paragraph = |rest: &str| rest.split("\n\n").next().unwrap_or(rest).trim().to_string();
    if let Some(rest) = text.strip_prefix(&format!("{BUDGET_EXHAUSTED}: ")) {
        return Some(first_paragraph(rest));
    }
    if text.starts_with("cap_exceeded: ") {
        let refusal = first_paragraph(text);
        let cap = refusal
            .split_once("cap $")
            .and_then(|(_, rest)| rest.split(';').next())
            .and_then(|figure| figure.trim().parse::<f64>().ok());
        return Some(format!("{}; {refusal}", budget_line(cap)));
    }
    if text.starts_with("budget_spent: usd") {
        return Some(format!("{}; budget_spent: usd", budget_line(None)));
    }
    None
}

/// What a budget-stopped run answers with, written without a model call:
/// the headline, then each delegation's work — the finished ones' answers
/// and the stopped ones' partial output — in the order they came back.
pub fn final_message(headline: &str, finished: &[(String, String)]) -> String {
    let best_of = headline.starts_with(BEST_OF_FAILED);
    let mut message = if best_of {
        format!("{headline}. best_of failed on every attempt; no answer was produced.")
    } else {
        format!("{headline}. Here is what was finished:")
    };
    if finished.is_empty() && !best_of {
        message.push_str("\n\nNothing was finished before the budget ran out.");
    }
    for (agent, work) in finished {
        message.push_str(&format!("\n\n**{agent}**\n{work}"));
    }
    message
}

/// Ends the run of a model whose `invoke_agent` came back with a spent
/// budget, at the model call that would have answered it, with
/// [`final_message`] as the reason after [`BUDGET_STOP`].
///
/// Every `invoke_agent` result of the run is noted on the way: a finished
/// delegation's answer, and a budget-stopped one's partial output, are
/// what the final message lists.
#[derive(Clone, Copy, Debug, Default)]
pub struct StopOnBudgetExhausted;

/// The run's delegations so far, and the stop's headline once one came
/// back budget-stopped.
#[derive(Clone, Default)]
struct BudgetStop {
    headline: Option<String>,
    finished: Vec<(String, String)>,
}

/// The agent and the work a successful `invoke_agent` result carries.
fn finished_work(rendered: &str) -> Option<(String, String)> {
    let output: serde_json::Value = serde_json::from_str(rendered).ok()?;
    Some((
        output["agent"].as_str()?.to_string(),
        output["response"].as_str()?.to_string(),
    ))
}

/// The agent and partial output a budget-stopped `invoke_agent` error
/// carries: the [`partial_output`] section, up to any tree messages
/// delivered after it.
fn partial_work(rendered: &str) -> Option<(String, String)> {
    let (_, rest) = rendered.split_once("\n\nWhat '")?;
    let (agent, rest) = rest.split_once("' finished:\n")?;
    let work = rest.split("\n\nmessages:\n").next().unwrap_or(rest).trim();
    (!work.is_empty()).then(|| (agent.to_string(), work.to_string()))
}

/// The section of a budget-stopped delegation's error that carries what
/// `agent` had streamed before it stopped; empty when it streamed nothing.
pub fn partial_output(agent: &str, partial: &str) -> String {
    let partial = partial.trim();
    if partial.is_empty() {
        String::new()
    } else {
        format!("\n\nWhat '{agent}' finished:\n{partial}")
    }
}

impl AgentHook for StopOnBudgetExhausted {
    async fn on_outcome(&self, ctx: &HookContext, event: OutcomeEvent<'_>) -> OutcomeAction {
        let Some(result) = event.tool_result() else {
            return OutcomeAction::Proceed;
        };
        let rendered = result.output().render();
        if event.tool_name() == Some(BestOfTool::NAME) {
            // Every solver failed (AGE-1132): the run ends here, not at the
            // model's next try at an answer of its own.
            if !result.is_success() && rendered.contains(BEST_OF_FAILED) {
                tracing::warn!("best_of failed on every attempt; ending the run");
                ctx.scratchpad().update(|stop: &mut BudgetStop| {
                    stop.headline
                        .get_or_insert_with(|| BEST_OF_FAILED.to_string());
                });
            }
            return OutcomeAction::Proceed;
        }
        if event.tool_name() != Some(InvokeAgentTool::NAME) {
            return OutcomeAction::Proceed;
        }
        ctx.scratchpad().update(|stop: &mut BudgetStop| {
            if result.is_success() {
                stop.finished.extend(finished_work(&rendered));
            } else if let Some(headline) = find_budget_exhausted(&rendered) {
                tracing::warn!(%headline, "A delegation ran out of budget; ending the run");
                stop.finished.extend(partial_work(&rendered));
                stop.headline.get_or_insert(headline);
            }
        });
        OutcomeAction::Proceed
    }

    async fn on_completion_call(
        &self,
        ctx: &HookContext,
        _event: CompletionCallEvent<'_>,
    ) -> CompletionCallAction {
        match ctx.scratchpad().get::<BudgetStop>() {
            Some(BudgetStop {
                headline: Some(headline),
                finished,
            }) => CompletionCallAction::stop(format!(
                "{BUDGET_STOP}{}",
                final_message(&headline, &finished)
            )),
            _ => CompletionCallAction::Continue,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shapes accepted as a budget refusal: hive's `402` body today
    /// (`error`), a `code` field, and OpenAI's nested envelope; nothing
    /// that is not a `402` naming one of the two codes.
    #[test]
    fn a_402_budget_refusal_is_read_from_its_body() {
        let hive = r#"{"error":"cap_exceeded","month_to_date":12.5,"cap_usd":10.0}"#;
        assert_eq!(
            endpoint_refusal(402, hive).as_deref(),
            Some(
                "budget_exhausted: Budget of $10.00 spent; the model endpoint refused the call \
                 (cap_exceeded)"
            )
        );
        let coded = r#"{"code":"budget_exceeded","message":"task budget spent"}"#;
        assert_eq!(
            endpoint_refusal(402, coded).as_deref(),
            Some(
                "budget_exhausted: Budget spent; the model endpoint refused the call \
                 (budget_exceeded): task budget spent"
            )
        );
        let nested = r#"{"error":{"code":"budget_exceeded","message":"m"},"budget_usd":2}"#;
        assert!(
            endpoint_refusal(402, nested)
                .unwrap()
                .starts_with("budget_exhausted: Budget of $2.00 spent")
        );
        assert_eq!(endpoint_refusal(429, hive), None);
        assert_eq!(endpoint_refusal(402, r#"{"error":"unpriced_usage"}"#), None);
        assert_eq!(endpoint_refusal(402, "payment required"), None);
    }

    /// Local parity (DP-3): the spend gate's refusals read as the same stop.
    #[test]
    fn a_spend_gate_refusal_is_a_budget_stop_too() {
        let wrapped = "Error: invoke_agent: budget_exhausted: Budget of $1.00 spent; x\n\nmore";
        assert_eq!(
            find_budget_exhausted(wrapped).as_deref(),
            Some("Budget of $1.00 spent; x")
        );
        let cap = crate::services::spend_gate::CapExceeded {
            month_to_date_usd: 12.5,
            cap_usd: 10.0,
        };
        assert!(
            find_budget_exhausted(&format!("Error: invoke_agent: {cap}"))
                .unwrap()
                .starts_with("Budget of $10.00 spent; cap_exceeded:")
        );
        assert_eq!(
            find_budget_exhausted("Error: invoke_agent: budget_spent: usd").as_deref(),
            Some("Budget spent; budget_spent: usd")
        );
        assert_eq!(find_budget_exhausted("budget_spent: turns"), None);
    }

    /// A failed delegation whose tree messages quote the gate is not a
    /// budget stop: the typed text has to lead the error.
    #[test]
    fn a_quoted_gate_refusal_in_tree_messages_is_not_a_budget_stop() {
        let quoted = "Error: invoke_agent: Agent 'w' reported failure: max turns\n\nmessages:\n\
                      [from sibling] the gate printed `cap_exceeded: month-to-date $1.00 >= cap \
                      $2.00; no delegation started`";
        assert_eq!(find_budget_exhausted(quoted), None);
        let spent = "Error: invoke_agent: Agent 'w' reported failure: x\n\nmessages:\n\
                     budget_spent: usd";
        assert_eq!(find_budget_exhausted(spent), None);
    }

    /// A remote agent's failure text that mentions the marker mid-text is
    /// its own words, not a typed stop — raw, as `finish` reads it, or
    /// wrapped, as the model and the frame mapper do.
    #[test]
    fn a_remote_failure_mentioning_the_marker_is_not_a_budget_stop() {
        let remote = "upstream says budget_exhausted: Budget of $1.00 spent";
        assert_eq!(find_budget_exhausted(remote), None);
        assert_eq!(
            find_budget_exhausted(&format!(
                "Error: invoke_agent: Agent 'remote' reported failure: {remote}"
            )),
            None
        );
    }

    /// A best_of stop says best_of failed and that no answer was produced,
    /// and never claims a budget is spent.
    #[test]
    fn best_of_stop_text_names_best_of() {
        let message = final_message(BEST_OF_FAILED, &[]);
        assert!(message.contains("best_of"), "{message}");
        assert!(message.contains("no answer was produced"), "{message}");
        assert!(!message.to_lowercase().contains("budget"), "{message}");
    }

    /// The budget stop's own text is pinned.
    #[test]
    fn budget_stop_text_unchanged() {
        assert_eq!(
            final_message("Budget of $1.00 spent; x", &[]),
            "Budget of $1.00 spent; x. Here is what was finished:\n\n\
             Nothing was finished before the budget ran out."
        );
        assert_eq!(
            final_message("Budget spent", &[("w".into(), "done".into())]),
            "Budget spent. Here is what was finished:\n\n**w**\ndone"
        );
    }

    /// The run itself ends (AGE-1132): a leader whose `best_of` failed on
    /// every attempt gets no further model call, and answers with the
    /// best_of final message.
    #[tokio::test]
    async fn a_failed_best_of_ends_the_run_at_the_next_call() {
        use futures::StreamExt;
        use rig_agent::agent::AgentBuilder;
        use rig_core::test_utils::{MockCompletionModel, MockStreamEvent};

        let model = MockCompletionModel::from_stream_turns([
            vec![
                MockStreamEvent::tool_call("t1", BestOfTool::NAME, serde_json::json!({}))
                    .with_call_id("call_1"),
                MockStreamEvent::final_response_with_total_tokens(4),
            ],
            vec![
                MockStreamEvent::text("my own answer"),
                MockStreamEvent::final_response_with_total_tokens(4),
            ],
        ]);
        let agent = AgentBuilder::new(model)
            .tool(FailedBestOf)
            .add_hook(StopOnBudgetExhausted)
            .build();
        let mut stream = agent.prompt("task").max_turns(4).stream();
        let mut reason = None;
        while let Some(item) = stream.next().await {
            if let Err(rig_agent::completion::PromptError::Cancelled { reason: r, .. }) = item {
                reason = Some(r);
            }
        }
        let reason = reason.expect("the run was stopped by the hook");
        assert_eq!(
            reason,
            format!("{BUDGET_STOP}{}", final_message(BEST_OF_FAILED, &[]))
        );
        assert!(!reason.contains("my own answer"));
    }

    /// A `best_of` that failed on every attempt, as the tool errors.
    struct FailedBestOf;

    impl Tool for FailedBestOf {
        const NAME: &'static str = BestOfTool::NAME;
        type Error = crate::tools::best_of_tool::BestOfError;
        type Args = serde_json::Value;
        type Output = String;

        fn description(&self) -> String {
            "fails".to_string()
        }

        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({ "type": "object", "properties": {} })
        }

        fn map_error(&self, error: Self::Error) -> rig_agent::tool::ToolExecutionError {
            crate::tools::map_tool_error(Self::NAME, error)
        }

        async fn call(
            &self,
            _context: &mut rig_agent::tool::ToolContext,
            _args: Self::Args,
        ) -> Result<Self::Output, Self::Error> {
            Err(crate::tools::best_of_tool::BestOfError::AllFailed(
                "a: x".into(),
            ))
        }
    }
}
