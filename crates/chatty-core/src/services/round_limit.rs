//! A cap on an agent's rounds of one fan-out (AGE-910).
//!
//! A research lead works in rounds: one model turn hands several
//! sub-questions to its researchers side by side, the next reads their
//! notes and re-plans. Its prompt says when to stop, but a prompt is not a
//! limit, and the issue's stop rule ("at round N") has to hold whatever the
//! model decides. A spec's `[swarm.rounds]` names the callee whose calls
//! make a round and the most rounds the agent may run:
//!
//! ```toml
//! [swarm.rounds]
//! agent = "deep-research-web-researcher"
//! max = 4
//! ```
//!
//! [`RoundLimit`] is the rig hook that holds it. A model turn that calls
//! `rounds.agent` (through `invoke_agent`) opens a round; every other call
//! to it in the same turn, the parallel fan-out, is part of that round. A
//! turn that would open round `max + 1` has each such call refused with
//! [`round_limit_note`] as its result, without starting the callee, and the
//! agent goes on: it delegates the write-up of what it has. Calls to any
//! other agent are never counted or refused.
//!
//! The count lives on the agent, not on one rig run, so a headless run's
//! follow-up passes (each a new run on the same history) share it. A new
//! user turn starts a new research task: the session calls
//! [`RoundLimit::reset`] there (AGE-1188), so a desktop conversation, which
//! builds its agent once, gives every question its own rounds.

use std::sync::Arc;

use parking_lot::Mutex;
use rig_agent::agent::{
    AgentHook, CompletionCallAction, CompletionCallEvent, DispatchAction, DispatchEvent,
    HookContext,
};
use rig_agent::tool::Tool;

use crate::agent_spec::RoundsSection;
use crate::tools::invoke_agent_tool::InvokeAgentTool;

/// What a refused call's result opens with, so a transcript, a test or the
/// scorecard can find it.
pub const ROUND_LIMIT_REACHED: &str = "round_limit";

/// The result a call past the cap gets instead of running.
pub fn round_limit_note(agent: &str, max: u32) -> String {
    format!(
        "{ROUND_LIMIT_REACHED}: all {max} rounds of '{agent}' are spent, so this call was not \
         started and no further one will be. Stop researching: hand the notes you have to the \
         writer now, and list the sub-questions still open as gaps."
    )
}

/// The rounds counted so far.
#[derive(Debug, Default)]
struct Rounds {
    /// Model calls started so far; a turn is known by its index.
    turn: u64,
    /// The turn that opened the latest round.
    round_turn: Option<u64>,
    /// Rounds opened.
    opened: u32,
}

/// The `[swarm.rounds]` hook (see the module docs). Cloning shares the
/// count. A `RoundLimit` without a config counts nothing and refuses
/// nothing: every agent's runs carry one.
#[derive(Clone, Debug, Default)]
pub struct RoundLimit {
    config: Option<RoundsSection>,
    rounds: Arc<Mutex<Rounds>>,
}

impl RoundLimit {
    pub fn new(config: Option<RoundsSection>) -> Self {
        Self {
            config,
            rounds: Arc::default(),
        }
    }

    /// The rounds opened so far.
    pub fn rounds(&self) -> u32 {
        self.rounds.lock().opened
    }

    /// A new user turn: the count starts over. Follow-up passes of the same
    /// task do not call this.
    pub fn reset(&self) {
        *self.rounds.lock() = Rounds::default();
    }

    /// A model call is starting: a new turn.
    pub(crate) fn next_turn(&self) {
        if self.config.is_some() {
            self.rounds.lock().turn += 1;
        }
    }

    /// Whether a tool call may run: `None` when it may, the refusal note
    /// when it would open a round past the cap.
    pub(crate) fn admit(&self, tool_name: &str, args: &str) -> Option<String> {
        let config = self.config.as_ref()?;
        if tool_name != InvokeAgentTool::NAME {
            return None;
        }
        let callee = serde_json::from_str::<serde_json::Value>(args)
            .ok()
            .and_then(|args| args["agent"].as_str().map(str::to_string))?;
        if callee != config.agent {
            return None;
        }
        let mut rounds = self.rounds.lock();
        if rounds.round_turn == Some(rounds.turn) {
            return None;
        }
        if rounds.opened >= config.max {
            tracing::info!(agent = %callee, max = config.max, "Round limit reached; call refused");
            return Some(round_limit_note(&callee, config.max));
        }
        rounds.opened += 1;
        rounds.round_turn = Some(rounds.turn);
        None
    }
}

impl AgentHook for RoundLimit {
    async fn on_completion_call(
        &self,
        _ctx: &HookContext,
        _event: CompletionCallEvent<'_>,
    ) -> CompletionCallAction {
        self.next_turn();
        CompletionCallAction::Continue
    }

    /// rig 0.46 gates a tool call at its dispatch (AGE-1151): a denied
    /// call never reaches the tool, so no researcher is spawned, and the
    /// model sees the note verbatim as the call's result.
    async fn on_dispatch(&self, _ctx: &HookContext, event: DispatchEvent<'_>) -> DispatchAction {
        let (Some(tool_name), Some(args)) = (event.tool_name(), event.tool_args()) else {
            return DispatchAction::proceed();
        };
        match self.admit(tool_name, args) {
            Some(note) => DispatchAction::skip(note),
            None => DispatchAction::proceed(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RESEARCHER: &str = "deep-research-web-researcher";

    fn limit(max: u32) -> RoundLimit {
        RoundLimit::new(Some(RoundsSection {
            agent: RESEARCHER.to_string(),
            max,
            notes: None,
        }))
    }

    fn call(agent: &str) -> String {
        serde_json::json!({ "agent": agent, "prompt": "p" }).to_string()
    }

    /// AGE-910: the parallel calls of one turn are one round; the turn
    /// that would open round `max + 1` has every researcher call refused,
    /// and calls to anyone else still run.
    #[test]
    fn a_turn_past_the_cap_is_refused_and_other_agents_are_not() {
        let limit = limit(2);
        for _ in 0..2 {
            limit.next_turn();
            for _ in 0..4 {
                assert_eq!(limit.admit("invoke_agent", &call(RESEARCHER)), None);
            }
        }
        assert_eq!(limit.rounds(), 2);

        // A turn without the researcher opens no round.
        limit.next_turn();
        assert_eq!(
            limit.admit("invoke_agent", &call("deep-research-writer")),
            None
        );
        assert_eq!(limit.admit("read_file", "{}"), None);
        assert_eq!(limit.rounds(), 2);

        limit.next_turn();
        let refused = limit
            .admit("invoke_agent", &call(RESEARCHER))
            .expect("round 3 is past the cap");
        assert!(
            refused.starts_with("round_limit: all 2 rounds"),
            "{refused}"
        );
        assert!(limit.admit("invoke_agent", &call(RESEARCHER)).is_some());
        assert_eq!(
            limit.admit("invoke_agent", &call("deep-research-writer")),
            None
        );
        assert_eq!(limit.rounds(), 2);
    }

    /// A clone is the same agent's count: a follow-up pass shares it.
    #[test]
    fn clones_share_the_count() {
        let first = limit(1);
        let pass = first.clone();
        first.next_turn();
        assert_eq!(first.admit("invoke_agent", &call(RESEARCHER)), None);
        pass.next_turn();
        assert!(pass.admit("invoke_agent", &call(RESEARCHER)).is_some());
    }

    /// Without `[swarm.rounds]` nothing is counted or refused.
    #[test]
    fn no_config_refuses_nothing() {
        let limit = RoundLimit::default();
        for _ in 0..10 {
            limit.next_turn();
            assert_eq!(limit.admit("invoke_agent", &call(RESEARCHER)), None);
        }
        assert_eq!(limit.rounds(), 0);
    }
}
