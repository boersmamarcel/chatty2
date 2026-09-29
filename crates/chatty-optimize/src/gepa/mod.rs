//! GEPA loop scaffolding (AGE-15).
//!
//! You fill [`evolve::evolve`] (Algorithm 1). Already wired: [`select::select_candidate`],
//! [`SelectBestCandidate`], [`evolve::maybe_merge`] (flag default off).
//! Still reserved: [`merge::merge`], [`prompts::REFLECTION_META_PROMPT`].
//!
//! # Reference
//!
//! This module implements (parts of) GEPA as described by:
//! Lakshya A. Agrawal et al., *GEPA: Reflective Prompt Evolution Can Outperform
//! Reinforcement Learning*, ICLR 2026 Oral.
//! <https://arxiv.org/abs/2507.19457>
//!
//! Mapping: Algorithm 1 / Figure 4 → [`evolve`]; Algorithm 2 / §3.3 → [`select`];
//! Appendix F (Algorithms 3–4) → [`merge`]; Appendix B → [`prompts`] (text reserved);
//! Observation 3 / Table 3 → [`SelectBestCandidate`].

pub mod evolve;
pub mod merge;
pub mod prompts;
pub mod select;
pub mod system;

use crate::OptimizeError;

/// Minibatch size `b = 3` (Agrawal et al., ICLR 2026, Algorithm 1 hyperparameters).
pub const DEFAULT_MINIBATCH: usize = 3;

/// Ablation: always pick the current best instead of Pareto sampling.
///
/// Paper: Observation 3 / Table 3 / Figure 6 (`SelectBestCandidate` vs Pareto).
/// Agrawal et al., arXiv:2507.19457.
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
