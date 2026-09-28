//! What one run has left to hand its callees (PL-S2 DP-3).
//!
//! A delegate runs under `min(own budget, caller's remaining)` for turns,
//! time and dollars. The broker holds each run's budget in its call chain,
//! but only the run itself knows what it has spent of it, so its
//! `invoke_agent` says so on every call ([`RunBudget::remaining`]) and the
//! broker narrows the chain with it. A run delegated to starts from what its
//! caller left it ([`RunBudget::narrow`]).
//!
//! The host keeps it current: the turn cap and the clock it starts the run
//! with, the tool turns spent, and the run's own usage lines. The dollar part
//! is the run's [`LocalSpendGate`]; `invoke_agent` records its callees' usage
//! there itself.

use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use chatty_fabric::Remaining;

use crate::models::token_usage::TokenUsage;
use crate::services::spend_gate::LocalSpendGate;

/// One run's budget and what it has spent of it. Clones share one record.
#[derive(Clone, Debug, Default)]
pub struct RunBudget {
    state: Arc<Mutex<State>>,
    spend: LocalSpendGate,
}

#[derive(Debug, Default)]
struct State {
    /// Tool turns; `None` is no cap.
    turns: Option<u32>,
    turns_spent: u32,
    deadline: Option<SystemTime>,
}

impl RunBudget {
    /// A run capped at `turns` tool turns (`None`: no cap) whose dollars
    /// are `spend`'s.
    pub fn new(turns: Option<u32>, spend: LocalSpendGate) -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                turns,
                ..State::default()
            })),
            spend,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The run's dollars.
    pub fn spend(&self) -> &LocalSpendGate {
        &self.spend
    }

    /// The tool-turn cap; `None` is none.
    pub fn turns(&self) -> Option<u32> {
        self.lock().turns
    }

    /// Lower the turn and dollar caps to what a caller left this run, where
    /// that is tighter. Time is the host's: it starts the run's clock with
    /// the tighter of its own budget and `ceiling.seconds`.
    pub fn narrow(&self, ceiling: &Remaining) {
        {
            let mut state = self.lock();
            let own = Remaining {
                turns: state.turns,
                ..Remaining::default()
            };
            state.turns = own.min(ceiling).turns;
        }
        if let Some(usd) = ceiling.usd {
            self.spend.narrow(usd);
        }
    }

    /// The run's clock started with `budget` left; `None` is no deadline.
    pub fn start_clock(&self, budget: Option<Duration>) {
        self.lock().deadline = budget.map(|budget| SystemTime::now() + budget);
    }

    /// The tool turns the run has spent so far.
    pub fn set_turns_spent(&self, turns: u32) {
        self.lock().turns_spent = turns;
    }

    /// Usage lines the run spent itself; its callees' are recorded by
    /// `invoke_agent`.
    pub fn record(&self, lines: impl IntoIterator<Item = TokenUsage>) {
        self.spend.record(lines);
    }

    /// What the run has left now: turns less those spent, seconds to its
    /// deadline, dollars less what its lines cost.
    pub fn remaining(&self) -> Remaining {
        let state = self.lock();
        Remaining {
            turns: state
                .turns
                .map(|turns| turns.saturating_sub(state.turns_spent)),
            seconds: state.deadline.map(|deadline| {
                deadline
                    .duration_since(SystemTime::now())
                    .unwrap_or_default()
                    .as_secs()
            }),
            usd: self.spend.remaining_usd(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::token_usage::PriceBook;

    #[test]
    fn a_run_hands_on_what_it_has_not_spent() {
        let budget = RunBudget::new(
            Some(10),
            LocalSpendGate::new(Some(1.0), PriceBook::default()),
        );
        assert_eq!(
            budget.remaining(),
            Remaining {
                turns: Some(10),
                seconds: None,
                usd: Some(1.0),
            }
        );
        budget.set_turns_spent(3);
        budget.start_clock(Some(Duration::from_secs(600)));
        let left = budget.remaining();
        assert_eq!(left.turns, Some(7));
        assert!(left.seconds.is_some_and(|s| (598..=600).contains(&s)));
    }

    #[test]
    fn a_delegated_run_starts_from_what_its_caller_left_it() {
        let budget = RunBudget::new(None, LocalSpendGate::default());
        assert!(budget.remaining().is_unlimited());
        budget.narrow(&Remaining {
            turns: Some(2),
            seconds: Some(30),
            usd: Some(0.02),
        });
        assert_eq!(budget.turns(), Some(2));
        assert_eq!(budget.spend().cap_usd(), Some(0.02));
        budget.narrow(&Remaining {
            turns: Some(5),
            ..Remaining::default()
        });
        assert_eq!(budget.turns(), Some(2), "narrowing never raises a cap");
    }
}
