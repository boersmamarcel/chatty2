//! Multi-module system seam for GEPA (AGE-15).
//!
//! Not reserved — wire chatty-core's agent loop here once `Trajectory` is human-written.

use chatty_trace::Trajectory;

use crate::OptimizeError;

/// A compound system GEPA optimizes (preamble(s) + rollout entry point).
pub trait CompoundSystem: Send + Sync {
    /// Module names in round-robin mutation order (e.g. `["preamble"]` for skeleton).
    fn module_ids(&self) -> &[&str];

    /// One end-to-end rollout for a single task instance.
    fn rollout(
        &self,
        preamble: &str,
        question_id: &str,
        question: &str,
    ) -> Result<Trajectory, OptimizeError>;
}

/// Placeholder until chatty-core loop wiring lands.
pub struct UnwiredCompoundSystem {
    modules: Vec<&'static str>,
}

impl UnwiredCompoundSystem {
    pub fn new(modules: Vec<&'static str>) -> Self {
        Self { modules }
    }
}

impl CompoundSystem for UnwiredCompoundSystem {
    fn module_ids(&self) -> &[&str] {
        &self.modules
    }

    fn rollout(
        &self,
        preamble: &str,
        question_id: &str,
        question: &str,
    ) -> Result<Trajectory, OptimizeError> {
        let _ = (self, preamble, question_id, question);
        Trajectory::from_capture("").map_err(|e| OptimizeError::InvalidInput(e.to_string()))
    }
}
