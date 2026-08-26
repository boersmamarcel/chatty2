//! ReAct strategy / termination backoff (AGE-11).
//!
//! `Strategy` is **reserved**. The loop itself already ships in chatty-core.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum StrategyError {
    #[error("strategy not implemented")]
    NotImplemented,
}

/// Eval-time regime switch (Act-only, CoT, CoT-SC, hybrids). Termination semantics reserved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Regime {
    ActOnly,
    ChainOfThought,
    ChainOfThoughtSelfConsistency,
    ReactThenCotSc,
    CotScThenReact,
}

/// Backoff / termination rule — human-reserved.
pub struct Strategy;

impl Strategy {
    pub fn should_stop(&self, _steps: usize, _regime: Regime) -> Result<bool, StrategyError> {
        todo!("human: Strategy — ReAct/CoT-SC backoff + termination semantics (AGE-11)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[should_panic(expected = "human: Strategy")]
    fn strategy_is_reserved() {
        let s = Strategy;
        let _ = s.should_stop(0, Regime::ActOnly);
    }
}
