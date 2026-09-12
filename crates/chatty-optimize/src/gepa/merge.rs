//! GEPA system-aware crossover (AGE-15).
//!
//! `merge` is **reserved**.
//!
//! # Reference
//!
//! Agrawal et al., *GEPA: Reflective Prompt Evolution Can Outperform Reinforcement
//! Learning*, ICLR 2026 Oral, arXiv:2507.19457. Appendix F, Algorithms 3 and 4.
//! Observation 5 discusses when merge helps vs hurts.
//! <https://arxiv.org/abs/2507.19457>

use crate::OptimizeError;

/// Merge two non-ancestral candidates via common ancestor + desirability + per-module pick.
///
/// Agrawal et al., Appendix F: `S[a] ≤ min(S[i], S[j])`, complementary module bits, ≤5 calls.
pub fn merge(
    score_i: f64,
    score_j: f64,
    score_ancestor: f64,
    modules_changed_i: &[bool],
    modules_changed_j: &[bool],
) -> Result<Vec<bool>, OptimizeError> {
    let _ = (
        score_i,
        score_j,
        score_ancestor,
        modules_changed_i,
        modules_changed_j,
    );
    todo!("human: merge — ancestry, desirability, per-module selection (AGE-15)")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[should_panic(expected = "human: merge")]
    fn merge_desirability_is_reserved() {
        let _ = merge(0.5, 0.6, 0.4, &[true, false], &[false, true]);
    }

    /// Spec: common ancestor with `S[a] ≤ min(S[i], S[j])` is eligible.
    /// Complementary module bits; implementation reserved.
    #[test]
    #[should_panic(expected = "human: merge")]
    fn merge_desirable_pair_is_reserved() {
        let _ = merge(0.6, 0.7, 0.4, &[true, false], &[false, true]);
    }

    /// Spec: ancestor stronger than both children is not desirable.
    #[test]
    #[should_panic(expected = "human: merge")]
    fn merge_strong_ancestor_is_reserved() {
        let _ = merge(0.5, 0.6, 0.9, &[true, false], &[false, true]);
    }
}
