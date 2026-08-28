//! GEPA Pareto candidate selection (AGE-15).
//!
//! `select_candidate` is **reserved** — GEPA's actual contribution (~30 lines).

use crate::OptimizeError;

/// Score matrix from the GEPA paper dominance example (rows = candidates, cols = instances).
///
/// Candidate 2 wins only instance 1; candidate 3 wins instances 1 and 2 → 2 is strictly dominated.
pub fn paper_dominance_matrix() -> Vec<Vec<f64>> {
    vec![
        vec![0.0, 0.0], // cand 0
        vec![1.0, 0.0], // cand 1
        vec![1.0, 0.0], // cand 2 — wins only instance 1
        vec![1.0, 1.0], // cand 3 — wins instances 1 and 2
    ]
}

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
        let matrix = paper_dominance_matrix();
        let _ = select_candidate(&matrix);
    }

    /// Manual debug while implementing (human-reserved).
    ///
    /// ```bash
    /// cargo test -p chatty-optimize manual_select_candidate -- --ignored --nocapture
    /// ```
    ///
    /// With lldb: set breakpoint on `select_candidate`, then run the same test under the debugger.
    #[test]
    #[ignore = "manual debug harness — run with --ignored while implementing select_candidate"]
    fn manual_select_candidate() {
        let matrix = paper_dominance_matrix();
        let idx = select_candidate(&matrix).expect("select_candidate");
        println!("selected candidate index: {idx}");
    }
}
