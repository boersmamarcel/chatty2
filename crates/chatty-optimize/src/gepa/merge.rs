//! GEPA system-aware crossover (AGE-15).
//!
//! `merge` is **reserved**.

use crate::OptimizeError;

/// Merge two non-ancestral candidates via common ancestor + desirability + per-module pick.
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
}
