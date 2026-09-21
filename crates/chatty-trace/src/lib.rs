//! Trace substrate for reflection (AGE-5 / AGE-22).
//!
//! # Promises (AGE-26 production bar)
//!
//! - Concrete error enum in public APIs (no `Box<dyn Error>`).
//! - No panics on input-driven paths.
//! - `Trajectory` retention behind a `Recorder` that no-ops by default in release.
//! - Does not pull Harbor / Stage B sandboxes into the app binary.
//! - `#![forbid(unsafe_code)]`.
//!
//! # Status
//!
//! Types `Trajectory`, `Step`, `Action`, `Outcome`, and `FeedbackFn` are **reserved**
//! ([`RESERVED.md`](../../../RESERVED.md)). Marcel defines them after the AGE-22 walking
//! skeleton exposes what ATIF v1.6 + rig-tap can attribute.

#![forbid(unsafe_code)]

pub mod feedback;
pub mod trace;

pub use feedback::{FeedbackFn, UnimplementedFeedback};
pub use trace::{Action, Outcome, Step, Trajectory};

use thiserror::Error;

/// Public error taxonomy for the crate (expanded as AGE-5 lands).
#[derive(Debug, Error)]
pub enum TraceError {
    #[error("trace not yet available: {0}")]
    NotReady(&'static str),
    #[error("serde error: {0}")]
    Serde(#[from] serde_json::Error),
}

/// Placeholder so the crate compiles before reserved types land.
pub fn crate_ready() -> Result<(), TraceError> {
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
