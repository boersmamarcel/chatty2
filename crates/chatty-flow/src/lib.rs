//! AFlow workflow representation and interpreter (AGE-7 / AGE-13).
//!
//! # Promises (AGE-26 production bar)
//!
//! - Interpreter is a shipping path: cancellation/timeouts when wired.
//! - Concrete error enum; no panics on input-driven paths.
//! - Does not pull Harbor / Stage B sandboxes into the app binary.
//! - `#![forbid(unsafe_code)]`.
//!
//! # Reserved
//!
//! `WorkflowRepr` and `WfNode` are human-reserved ([`RESERVED.md`](../../../RESERVED.md)).

#![forbid(unsafe_code)]

pub mod interpreter;
pub mod ir;
pub mod repr;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum FlowError {
    #[error("flow not yet available: {0}")]
    NotReady(&'static str),
    #[error("invalid workflow: {0}")]
    Invalid(String),
}

pub fn crate_ready() -> Result<(), FlowError> {
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
