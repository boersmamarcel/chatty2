//! GEPA Pareto candidate selection (AGE-15).
//!
//! `select_candidate` is **reserved** — GEPA's actual contribution (~30 lines).

use crate::OptimizeError;

/// Instance-wise best → candidates winning ≥1 instance → strict-dominance prune → sample ∝ wins.
pub fn select_candidate(score_matrix: &[Vec<f64>]) -> Result<usize, OptimizeError> {
    let _ = score_matrix;
    todo!("human: select_candidate — instance-wise Pareto + frequency sampling (AGE-15)")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Paper-shaped dominance example: candidate 2 wins only task 1; candidate 3 wins
    /// tasks 1 and 2 → candidate 2 is pruned. Implementation reserved.
    #[test]
    #[should_panic(expected = "human: select_candidate")]
    fn dominance_example_is_reserved() {
        // rows = candidates, cols = instances
        let matrix = vec![
            vec![0.0, 0.0], // cand 0
            vec![1.0, 0.0], // cand 1 — best on task 0 only in a fuller example
            vec![1.0, 0.0], // cand 2 — wins only task 1 in the paper write-up
            vec![1.0, 1.0], // cand 3 — wins tasks 1 and 2
        ];
        let _ = select_candidate(&matrix);
    }
}
