//! Optimizers: GEPA Pareto prompt evolution and AFlow MCTS (AGE-7 / AGE-8 / AGE-15).
//!
//! Held to "correct", not the full AGE-26 production bar — but must not pull `chatty-eval`
//! into any shipping binary. Invoked on demand or in CI, never on a request path.
//!
//! # Reserved
//!
//! `SelectionStrategy`, `soft_mixed_select`, `select_candidate`, `merge`, and
//! `REFLECTION_META_PROMPT` are human-reserved ([`RESERVED.md`](../../../RESERVED.md)).

#![forbid(unsafe_code)]

pub mod aflow;
pub mod archive;
pub mod gepa;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum OptimizeError {
    #[error("optimize not yet available: {0}")]
    NotReady(&'static str),
    #[error("invalid selection input: {0}")]
    InvalidInput(String),
}

pub fn crate_ready() -> Result<(), OptimizeError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiles() {
        crate_ready().unwrap();
    }
}
