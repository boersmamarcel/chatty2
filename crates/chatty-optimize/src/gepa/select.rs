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
use rand::distributions::{Distribution, WeightedIndex};
use rand::thread_rng;

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
///
/// Paper Algorithm 2 (`SELECTCANDIDATE`): `s*` → `P*` → `C` → `D` → `Ĉ` → `f` → sample.
// HUMAN-WRITTEN: select_candidate
pub fn select_candidate(score_matrix: &[Vec<f64>]) -> Result<usize, OptimizeError> {
    let (_, weights) = pareto_candidates_and_win_counts(score_matrix)?;
    let dist =
        WeightedIndex::new(&weights).map_err(|e| OptimizeError::InvalidInput(e.to_string()))?;
    // `weights` is indexed by candidate id, so the sample *is* the candidate.
    Ok(dist.sample(&mut thread_rng()))
}

/// Returns `(Ĉ, weights)`: survivors, and a length-`n` weight vector (index = candidate id).
fn pareto_candidates_and_win_counts(
    score_matrix: &[Vec<f64>],
) -> Result<(Vec<usize>, Vec<f64>), OptimizeError> {
    let n = score_matrix.len();
    let n_inst = score_matrix
        .first()
        .map(|row| row.len())
        .ok_or_else(|| OptimizeError::InvalidInput("empty candidate pool".into()))?;
    if n_inst == 0 {
        return Err(OptimizeError::InvalidInput(
            "candidates have no instance scores".into(),
        ));
    }
    if score_matrix.iter().any(|row| row.len() != n_inst) {
        return Err(OptimizeError::InvalidInput("ragged score matrix".into()));
    }

    // s*[i] = max_k S[k][i]  — best score seen on each instance
    let mut s_star = score_matrix[0].clone();
    for row in &score_matrix[1..] {
        for (i, &score) in row.iter().enumerate() {
            if score > s_star[i] {
                s_star[i] = score;
            }
        }
    }

    // P*[i] = { k | S[k][i] = s*[i] }  — every candidate that *ties* for best on instance i
    let mut p_star: Vec<Vec<usize>> = vec![Vec::new(); n_inst];
    for (k, row) in score_matrix.iter().enumerate() {
        for (i, &score) in row.iter().enumerate() {
            if score == s_star[i] {
                p_star[i].push(k);
            }
        }
    }

    // C = unique candidates that win (or tie for) at least one instance
    // p_star.iter().flatten() walks the inner lists in order and yields candidate indices, not scores:
    // 1, 2, 3, 3
    // Each yield is k. in_c[k] = true means “this candidate showed up at least once.” Setting true twice is a no-op. That is the uniqueness: membership, not a count.

    // k=1 → [false, true,  false, false]
    // k=2 → [false, true,  true,  false]
    // k=3 → [false, true,  true,  true ]
    // k=3 → [false, true,  true,  true ]   // already true
    let mut in_c = vec![false; n];
    for &k in p_star.iter().flatten() {
        in_c[k] = true;
    }

    // D: strictly dominated members of C. Φ_j dominates Φ_i iff
    // S[j][t] ≥ S[i][t] for every instance t, and > for at least one t.
    // Checked only among C, not the full pool.
    let mut dominated = vec![false; n];
    for i in 0..n {
        if !in_c[i] {
            continue;
        }
        for j in 0..n {
            if i != j && in_c[j] && strictly_dominates(&score_matrix[j], &score_matrix[i]) {
                dominated[i] = true;
                // there is no need to check the remainder as it is already dominated by one
                break;
            }
        }
    }

    // Ĉ = C \ D
    let c_hat: Vec<usize> = (0..n).filter(|&k| in_c[k] && !dominated[k]).collect();
    if c_hat.is_empty() {
        return Err(OptimizeError::InvalidInput(
            "Pareto front is empty after dominance pruning".into(),
        ));
    }

    // f[Φ] = |{ i | Φ ∈ P*[i] }|  — win *count*, not sum of scores
    // Paper matrix, step by step
    //         inst 0   inst 1
    // 0        0        0
    // 1        1        0
    // 2        1        0
    // 3        1        1
    // s*     = 1        1
    // p_star = [1,2,3]  [3]
    // Start: f = [0, 0, 0, 0]

    // Instance 0, winners = [1, 2, 3]:

    // f[1] += 1  →  [0, 1, 0, 0]
    // f[2] += 1  →  [0, 1, 1, 0]
    // f[3] += 1  →  [0, 1, 1, 1]
    //
    // It just simply adds 1 for the instance wins

    let mut f = vec![0.0; n];
    for winners in &p_star {
        // note that p_star are already all winners
        for &k in winners {
            f[k] += 1.0;
        }
    }

    // `f` is already the win-count, indexed by candidate id — including dominated
    // winners, whose `f` is > 0. Zero everyone not in Ĉ so WeightedIndex cannot
    // pick them. Never-winners are already 0.
    for (k, weight) in f.iter_mut().enumerate() {
        if !in_c[k] || dominated[k] {
            *weight = 0.0;
        }
    }
    Ok((c_hat, f))
}

fn strictly_dominates(a: &[f64], b: &[f64]) -> bool {
    let mut strictly_better = false;
    for (&ai, &bi) in a.iter().zip(b) {
        // no item can be smaller
        if ai < bi {
            return false;
        }
        // it should score higher for at least one item
        if ai > bi {
            strictly_better = true;
        }
    }
    strictly_better
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Paper-shaped dominance example: candidate 2 wins only task 1; candidate 3 wins
    /// tasks 1 and 2 → candidate 2 is pruned.
    #[test]
    fn dominance_example_prunes_candidate_2() {
        let matrix = paper_dominance_matrix();
        let (c_hat, weights) = pareto_candidates_and_win_counts(&matrix).unwrap();
        assert_eq!(c_hat, vec![3], "only candidate 3 survives the front");
        assert_eq!(
            weights,
            vec![0.0, 0.0, 0.0, 2.0],
            "dominated winners are zeroed; candidate 3 wins both instances"
        );
        for _ in 0..32 {
            assert_eq!(select_candidate(&matrix).unwrap(), 3);
        }
    }

    /// Complementary winners: neither dominates, sample weights equal the win counts.
    #[test]
    fn selection_weights_are_win_counts() {
        let matrix = vec![
            vec![1.0, 1.0, 0.0], // cand 0 wins instances 0 and 1
            vec![0.0, 0.0, 1.0], // cand 1 wins instance 2
        ];
        let (c_hat, weights) = pareto_candidates_and_win_counts(&matrix).unwrap();
        assert_eq!(c_hat, vec![0, 1]);
        assert_eq!(weights, vec![2.0, 1.0]);
    }

    #[test]
    fn empty_matrix_is_invalid() {
        let err = select_candidate(&[]).unwrap_err();
        assert!(matches!(err, OptimizeError::InvalidInput(_)));
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
