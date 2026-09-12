//! Trace substrate types (AGE-5 / AGE-22).
//!
//! `Trajectory`, `Step`, `Action`, and `Outcome` are **reserved** — Marcel defines
//! the contract after running the walking skeleton against real ATIF / rig-tap output.
//! Do not implement these symbols here; leave the `todo!("human: …")` markers.

use crate::TraceError;

/// One rollout trace — the contract every optimizer is written against.
pub struct Trajectory;

impl Trajectory {
    /// Build from captured run data (ATIF JSON, rig-tap events, or both — human decides).
    pub fn from_capture(_raw: &str) -> Result<Self, TraceError> {
        todo!("human: Trajectory — contract every optimizer is written against (AGE-22 / AGE-5)")
    }

    /// Steps in visitation order for reflection / feedback.
    pub fn steps(&self) -> Result<Vec<Step>, TraceError> {
        let _ = self;
        todo!("human: Trajectory — contract every optimizer is written against (AGE-22 / AGE-5)")
    }
}

/// One thought / action / observation triple in a rollout.
pub struct Step;

impl Step {
    /// Which module produced this step (for per-prompt attribution).
    pub fn module_id(&self) -> Result<&str, TraceError> {
        let _ = self;
        todo!("human: Step — one thought/action/observation triple (AGE-5)")
    }
}

/// ReAct action space `Â = A ∪ L` encoded in the type system.
pub enum Action {
    Placeholder,
}

impl Action {
    /// Classify one captured step into language vs environment action.
    pub fn classify_step(_step: &Step) -> Result<Self, TraceError> {
        let _ = _step;
        todo!("human: Action — Â = A ∪ L in the type system (AGE-22 / AGE-5)")
    }
}

/// Rollout success — not a bare bool.
pub enum Outcome {
    Placeholder,
}

impl Outcome {
    pub fn from_trajectory(_trajectory: &Trajectory) -> Result<Self, TraceError> {
        todo!("human: Outcome — what rollout success means; not a bool (AGE-5)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[should_panic(expected = "human: Trajectory")]
    fn trajectory_from_capture_is_reserved() {
        let _ = Trajectory::from_capture("{}");
    }

    #[test]
    #[should_panic(expected = "human: Step")]
    fn step_module_id_is_reserved() {
        let step = Step;
        let _ = step.module_id();
    }

    #[test]
    #[should_panic(expected = "human: Action")]
    fn action_classify_is_reserved() {
        let _ = Action::classify_step(&Step);
    }

    #[test]
    #[should_panic(expected = "human: Outcome")]
    fn outcome_from_trajectory_is_reserved() {
        let t = Trajectory;
        let _ = Outcome::from_trajectory(&t);
    }
}
