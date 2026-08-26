//! GEPA loop scaffolding (AGE-15). Reserved: select_candidate, merge, REFLECTION_META_PROMPT.

pub mod merge;
pub mod prompts;
pub mod select;

use crate::OptimizeError;

/// Minibatch size from the paper (b = 3).
pub const DEFAULT_MINIBATCH: usize = 3;

/// Ablation: always pick the current best instead of Pareto sampling.
#[derive(Debug, Clone, Copy, Default)]
pub struct SelectBestCandidate;

impl SelectBestCandidate {
    pub fn select(&self, mean_scores: &[f64]) -> Result<usize, OptimizeError> {
        mean_scores
            .iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(i, _)| i)
            .ok_or(OptimizeError::NotReady("empty candidate pool"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn select_best_picks_argmax() {
        let s = SelectBestCandidate;
        assert_eq!(s.select(&[0.1, 0.9, 0.5]).unwrap(), 1);
    }
}
