//! A worker that never started is the user's to fix, not its caller's
//! (AGE-822).
//!
//! A delegation can fail before the callee does anything: its worktree
//! cannot be made, its process cannot be spawned, it never says hello. None
//! of that is something the calling model can repair — on v0.5.7 a lead
//! that got one explained git branches to the user, retried, and told him
//! to create a branch by hand. So the broker types the failure
//! ([`chatty_fabric::WORKER_START_FAILED`]), and here:
//!
//! - **The caller stops.** `invoke_agent` returns it as a terminal error
//!   ([`InvokeAgentError::WorkerStartFailed`](crate::tools::invoke_agent_tool::InvokeAgentError)),
//!   and [`StopOnWorkerStartFailure`] ends the calling model's run on that
//!   result: no retry, no explanation. The run ends with the error
//!   [`card`](WorkerStartFailure::card) as its answer, so a sub-leader
//!   hands the same failure up, and the root shows it.
//! - **The user is told what failed and what to do**, in a card the
//!   desktop draws as an error alert ([`parse_card`]). `/agent`'s own
//!   preflight — no workspace, no code execution — uses the same card
//!   ([`preflight_card`]) before anything is spawned.

use chatty_fabric::find_worker_start_failure;
use rig_agent::agent::{
    AgentHook, CompletionCallAction, CompletionCallEvent, HookContext, ToolResultAction,
    ToolResultEvent,
};
use rig_agent::tool::Tool;

use crate::tools::invoke_agent_tool::InvokeAgentTool;

/// What the calling model reads under the failure: it is terminal.
pub const STOP_NOTE: &str = "The user has been shown this error. It is a setup problem only \
     the user can fix: do not retry the call, do not work around it, and do not give setup \
     advice. Stop here.";

/// The reason [`StopOnWorkerStartFailure`] stops a run with, ahead of the
/// card; `llm_service` ends such a run normally with the card as its text.
pub const WORKER_START_STOP: &str = "worker start failed\n";

/// The first words of every card, which is how the transcript knows one.
pub const CARD_TITLE: &str = "\u{26d4} Could not start";

/// A worker that never started: who, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkerStartFailure {
    pub agent: String,
    pub reason: String,
}

impl WorkerStartFailure {
    /// The failure `text` carries, wherever in it — a tool error, a
    /// sub-leader's failed task — or `None`.
    pub fn find(text: &str) -> Option<Self> {
        find_worker_start_failure(text).map(|(agent, reason)| Self {
            agent: agent.to_string(),
            reason: reason.to_string(),
        })
    }

    /// The card the user is shown: what failed, and what to do about it.
    pub fn card(&self) -> String {
        card(&self.agent, &self.reason, advice(&self.reason))
    }
}

/// What the user can do about a start failure, by what failed.
fn advice(reason: &str) -> &'static str {
    if reason.contains("no commits yet") {
        "This team gives each agent its own git worktree (\"isolate\": true), and a \
         repository needs a first commit for that. Commit something in the workspace, or \
         pick a folder that is not a git repository, then send the message again."
    } else if reason.contains("worktree") {
        "This team gives each agent its own git worktree (\"isolate\": true in its \
         team.json). Check that the workspace is a working git repository, or use a team \
         without \"isolate\", then send the message again."
    } else if reason.contains("register") || reason.contains("failed to spawn") {
        "The agent's process stopped before it could take the task. Check the agent's model \
         and provider in Settings and that the model server is running, then send the \
         message again."
    } else {
        "Fix the problem above, then send the message again."
    }
}

/// The card `/agent` shows when it refuses before spawning anything.
pub fn preflight_card(agent: &str, what_failed: &str, what_to_do: &str) -> String {
    card(agent, what_failed, what_to_do)
}

fn card(agent: &str, what_failed: &str, what_to_do: &str) -> String {
    format!("{CARD_TITLE} '{agent}'\n\nWhat failed: {what_failed}\n\nWhat to do: {what_to_do}")
}

/// A card's title line and the rest, when `text` is one.
pub fn parse_card(text: &str) -> Option<(&str, &str)> {
    let text = text.trim();
    if !text.starts_with(CARD_TITLE) {
        return None;
    }
    Some(match text.split_once("\n\n") {
        Some((title, body)) => (title, body.trim()),
        None => (text, ""),
    })
}

/// Ends the run of a model whose `invoke_agent` call failed to start its
/// worker, with the card as the reason after [`WORKER_START_STOP`].
///
/// The failed result itself goes through — into the history, so the next
/// turn's request pairs every call with its result, and onto the stream, so
/// the transcript shows it and a sub-leader's terminal status fails with it
/// — and the run stops at the model call that would have answered it.
#[derive(Clone, Copy, Debug, Default)]
pub struct StopOnWorkerStartFailure;

/// The card a run is to stop with, kept between the failed result and the
/// next model call.
#[derive(Clone)]
struct PendingStop(String);

impl AgentHook for StopOnWorkerStartFailure {
    async fn on_tool_result(
        &self,
        ctx: &HookContext,
        event: ToolResultEvent<'_>,
    ) -> ToolResultAction {
        if event.tool_name == InvokeAgentTool::NAME
            && let Some(failure) = WorkerStartFailure::find(&event.presentation.render())
        {
            tracing::warn!(
                agent = %failure.agent,
                reason = %failure.reason,
                "A delegated worker could not start; ending the run with the error"
            );
            ctx.scratchpad().insert(PendingStop(failure.card()));
        }
        ToolResultAction::Keep
    }

    async fn on_completion_call(
        &self,
        ctx: &HookContext,
        _event: CompletionCallEvent<'_>,
    ) -> CompletionCallAction {
        match ctx.scratchpad().get::<PendingStop>() {
            Some(PendingStop(card)) => {
                CompletionCallAction::stop(format!("{WORKER_START_STOP}{card}"))
            }
            None => CompletionCallAction::Continue,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_card_says_what_failed_and_what_to_do() {
        let failure = WorkerStartFailure::find(&chatty_fabric::worker_start_failed(
            "data-analyst",
            "failed to prepare a workspace for worker 'data-analyst-0': the workspace /r is \
             a git repository with no commits yet, so there is no branch to give it a worktree on",
        ))
        .expect("found");
        let card = failure.card();
        let (title, body) = parse_card(&card).expect("a card");
        assert_eq!(title, "\u{26d4} Could not start 'data-analyst'");
        assert!(
            body.contains("What failed: failed to prepare a workspace"),
            "{body}"
        );
        assert!(body.contains("Commit something in the workspace"), "{body}");
        assert!(
            !card.contains("worker_start_failed"),
            "no wire vocabulary: {card}"
        );
        assert_eq!(parse_card("An ordinary answer."), None);
    }
}
