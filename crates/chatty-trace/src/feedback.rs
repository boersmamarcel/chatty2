//! GEPA feedback function `µ_f` (AGE-5 / AGE-22).
//!
//! `FeedbackFn` is **reserved**. Scalar-only feedback is explicitly out of scope.

use crate::{Outcome, TraceError, Trajectory};

/// Natural-language feedback for reflective mutation (`µ_f` in GEPA).
///
/// Human defines the signature and return shape after the walking skeleton exposes
/// what ATIF + rig-tap can and cannot attribute.
pub trait FeedbackFn: Send + Sync {
    /// Evaluate one rollout; returns text for the reflector, not just a scalar.
    fn evaluate(&self, trajectory: &Trajectory, outcome: &Outcome) -> Result<String, TraceError> {
        let _ = (trajectory, outcome);
        todo!("human: FeedbackFn — GEPA's µ_f; natural-language feedback (AGE-22 / AGE-5)")
    }
}

/// Placeholder impl so the trait path compiles; method stays reserved.
pub struct UnimplementedFeedback;

impl FeedbackFn for UnimplementedFeedback {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[should_panic(expected = "human: FeedbackFn")]
    fn feedback_fn_evaluate_is_reserved() {
        let f = UnimplementedFeedback;
        let t = Trajectory;
        let o = Outcome::Placeholder;
        let _ = f.evaluate(&t, &o);
    }
}
