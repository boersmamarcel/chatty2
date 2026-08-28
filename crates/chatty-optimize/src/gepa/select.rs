//! GEPA Pareto candidate selection (AGE-15).
//!
//! `select_candidate` is **reserved** — GEPA's actual contribution (~30 lines).
//!
//! # Debugging your implementation
//!
//! **Prefer the example binary** (always runs, shows `println!` inside `select_candidate`):
//!
//! ```bash
//! cargo run -p chatty-optimize --example select_candidate_debug
//! ```
//!
//! Or the ignored unit test (only exists after the debug harness is merged):
//!
//! ```bash
//! cargo test -p chatty-optimize gepa::select::tests::manual_select_candidate -- --ignored --nocapture
//! ```
//!
//! If you see `running 0 tests`, that test is not in your branch yet — use the example,
//! or cherry-pick commit `0d45dcb` from `cursor/age-22-walking-skeleton-e949`.

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
    // score_matrix[candidate][instance]
    let _ = score_matrix;
    // todo!("human: select_candidate — instance-wise Pareto + frequency sampling (AGE-15)");

    for (i, row) in score_matrix.iter().enumerate() {
        println!("candate:{i}, {row:?}");
    }

    Ok(0)
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
    /// Full name filter avoids accidentally matching zero tests:
    /// `cargo test -p chatty-optimize gepa::select::tests::manual_select_candidate -- --ignored --nocapture`
    #[test]
    #[ignore = "manual debug harness — run with --ignored while implementing select_candidate"]
    fn manual_select_candidate() {
        let matrix = paper_dominance_matrix();
        let idx = select_candidate(&matrix).expect("select_candidate");
        println!("selected candidate index: {idx}");
    }

    /// Prints the paper matrix only — always runnable, no `select_candidate` call.
    #[test]
    fn paper_matrix_debug_print() {
        let matrix = paper_dominance_matrix();
        for (i, row) in matrix.iter().enumerate() {
            println!("candidate {i}: {row:?}");
        }
    }
}
