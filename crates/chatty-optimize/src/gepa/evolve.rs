//! GEPA Algorithm 1 loop (AGE-15).
//!
//! `evolve` is the control flow you fill in. Helpers below are already implemented:
//! parent selection (Pareto vs ablation), round-robin module, minibatch sample,
//! scoring, merge flag (default off; `merge` itself stays reserved).
//!
//! # Reference
//!
//! Agrawal et al., *GEPA: Reflective Prompt Evolution Can Outperform Reinforcement
//! Learning*, ICLR 2026 Oral, arXiv:2507.19457. Algorithm 1 and Figure 4 (left).
//! <https://arxiv.org/abs/2507.19457>
//!
//! # Debugging
//!
//! ```bash
//! cargo run -p chatty-optimize --example evolve_debug
//! cargo test -p chatty-optimize gepa::evolve::tests::manual_evolve -- --ignored --nocapture
//! ```
//!
//! Algorithm 1 data structures the body should touch:
//! - `P` / `state.candidates`, `A` / `state.parents`, `S` / `state.scores`
//! - minibatch of size [`DEFAULT_MINIBATCH`] from `d_feedback`
//! - accept on minibatch mean; **then** full `d_pareto` into `S`
//! - `select_candidate` (or [`SelectBestCandidate`] when `config.select_best`)
//! - `reflection_meta_prompt()` at the UpdatePrompt call site (reserved text; Appendix B)
//! - `maybe_merge` only if `config.merge` (default `false`, ≤5 invocations; Appendix F)

use super::merge::merge;
use super::select::select_candidate;
use super::system::CompoundSystem;
use super::{DEFAULT_MINIBATCH, SelectBestCandidate};
use crate::OptimizeError;
use rand::seq::SliceRandom;
use rand::thread_rng;

/// Rollout budget and ablation knobs. Merge defaults **off**.
///
/// Hyperparameters from Agrawal et al., Algorithm 1 (`B`, `b`) and Appendix F (merge cap).
#[derive(Debug, Clone)]
pub struct GepaConfig {
    /// Total rollouts `B` (feedback + pareto combined, as you choose to count).
    pub budget: u32,
    /// Paper `b = 3`.
    pub minibatch: usize,
    /// System-aware crossover. Default off.
    pub merge: bool,
    /// Cap from the paper (≤5).
    pub max_merge_invocations: u32,
    /// Ablation: [`SelectBestCandidate`] instead of Pareto [`select_candidate`].
    pub select_best: bool,
}

impl Default for GepaConfig {
    fn default() -> Self {
        Self {
            budget: 12,
            minibatch: DEFAULT_MINIBATCH,
            merge: false,
            max_merge_invocations: 5,
            select_best: false,
        }
    }
}

/// Train-vs-validation rollout split (paper: report them separately).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RolloutCounts {
    /// Minibatch / `D_feedback` rollouts.
    pub feedback: u32,
    /// Full `D_pareto` rollouts (only on accepted children).
    pub pareto: u32,
}

impl RolloutCounts {
    pub fn total(self) -> u32 {
        self.feedback + self.pareto
    }
}

/// Candidate pool `P`, ancestry `A`, instance-wise scores `S` on `D_pareto`.
#[derive(Debug, Clone)]
pub struct GepaState<S> {
    pub candidates: Vec<S>,
    pub parents: Vec<Option<usize>>,
    /// `scores[k][i]` = `µ(P[k], d_pareto[i])`.
    pub scores: Vec<Vec<f64>>,
    /// Which module each child rewrote (for merge). Seed row is all-false.
    pub modules_changed: Vec<Vec<bool>>,
    pub rollouts: RolloutCounts,
    pub merge_invocations: u32,
}

/// Outcome of Algorithm 1: best-on-pareto-mean candidate plus the archive.
#[derive(Debug, Clone)]
pub struct GepaResult<S> {
    pub best_index: usize,
    pub best: S,
    pub state: GepaState<S>,
}

/// Algorithm 1 (`SELECTCANDIDATE` is line 7; this function is the outer loop).
///
/// Agrawal et al., ICLR 2026, Figure 4 (left) / Algorithm 1. Fill this in — see
/// the module docs for the data structures.
pub fn evolve<S: CompoundSystem>(
    seed: S,
    d_feedback: &[String],
    d_pareto: &[String],
    config: &GepaConfig,
) -> Result<GepaResult<S>, OptimizeError> {
    let _ = (seed, d_feedback, d_pareto, config);
    todo!("human: Algorithm 1 — pool P, minibatch gate, D_pareto on accept, ancestry (AGE-15)");
}

/// Pareto [`select_candidate`] (Algorithm 2) or mean-argmax ablation (Observation 3).
pub fn select_parent(scores: &[Vec<f64>], select_best: bool) -> Result<usize, OptimizeError> {
    if scores.is_empty() {
        return Err(OptimizeError::InvalidInput("empty candidate pool".into()));
    }
    if select_best {
        let means: Vec<f64> = scores.iter().map(|row| mean(row)).collect();
        SelectBestCandidate.select(&means)
    } else {
        select_candidate(scores)
    }
}

/// Round-robin module index (Algorithm 1 `SELECTMODULE`).
pub fn select_module(iteration: usize, n_modules: usize) -> Result<usize, OptimizeError> {
    if n_modules == 0 {
        return Err(OptimizeError::InvalidInput("system has no modules".into()));
    }
    Ok(iteration % n_modules)
}

/// Uniform minibatch of size `b` (or the whole pool if smaller).
pub fn sample_minibatch(d_feedback: &[String], b: usize) -> Vec<String> {
    if d_feedback.is_empty() || b == 0 {
        return Vec::new();
    }
    let n = b.min(d_feedback.len());
    let mut rng = thread_rng();
    d_feedback.choose_multiple(&mut rng, n).cloned().collect()
}

pub fn score_all<S: CompoundSystem>(sys: &S, instances: &[String]) -> Vec<f64> {
    instances.iter().map(|x| sys.evaluate(x)).collect()
}

pub fn mean(xs: &[f64]) -> f64 {
    if xs.is_empty() {
        0.0
    } else {
        xs.iter().sum::<f64>() / xs.len() as f64
    }
}

/// No-op when merge is off or the invocation cap is hit. Otherwise calls reserved [`merge`].
///
/// Appendix F: at most 5 merge invocations; default off in this crate.
pub fn maybe_merge(
    config: &GepaConfig,
    invocations: &mut u32,
    score_i: f64,
    score_j: f64,
    score_ancestor: f64,
    changed_i: &[bool],
    changed_j: &[bool],
) -> Result<Option<Vec<bool>>, OptimizeError> {
    if !config.merge || *invocations >= config.max_merge_invocations {
        return Ok(None);
    }
    *invocations += 1;
    Ok(Some(merge(
        score_i,
        score_j,
        score_ancestor,
        changed_i,
        changed_j,
    )?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gepa::select::paper_dominance_matrix;
    use crate::gepa::system::{DualKeywordSystem, KeywordSystem};

    #[test]
    #[should_panic(expected = "human: Algorithm 1")]
    fn evolve_is_a_stub() {
        let seed = KeywordSystem::new("seed");
        let feedback = vec!["a".into()];
        let pareto = vec!["a".into(), "b".into()];
        let _ = evolve(seed, &feedback, &pareto, &GepaConfig::default());
    }

    #[test]
    fn select_parent_paper_matrix_always_picks_3() {
        let matrix = paper_dominance_matrix();
        for _ in 0..16 {
            assert_eq!(select_parent(&matrix, false).unwrap(), 3);
        }
    }

    #[test]
    fn select_parent_ablation_picks_highest_mean() {
        let scores = vec![vec![0.0, 0.0], vec![1.0, 1.0], vec![0.5, 0.5]];
        assert_eq!(select_parent(&scores, true).unwrap(), 1);
    }

    #[test]
    fn select_module_round_robins() {
        assert_eq!(select_module(0, 2).unwrap(), 0);
        assert_eq!(select_module(1, 2).unwrap(), 1);
        assert_eq!(select_module(2, 2).unwrap(), 0);
    }

    #[test]
    fn same_evolve_signature_accepts_both_systems() {
        fn assert_evolve<S: CompoundSystem>(_: S) {}
        assert_evolve(KeywordSystem::new("x"));
        assert_evolve(DualKeywordSystem::new("x", "y"));
    }

    #[test]
    fn score_all_matches_evaluate() {
        let sys = KeywordSystem::new("cat dog");
        let inst = vec!["cat".into(), "bird".into()];
        assert_eq!(score_all(&sys, &inst), vec![1.0, 0.0]);
        assert_eq!(mean(&[1.0, 0.0]), 0.5);
    }

    #[test]
    fn maybe_merge_is_noop_when_disabled() {
        let cfg = GepaConfig::default();
        assert!(!cfg.merge);
        let mut n = 0;
        let out = maybe_merge(&cfg, &mut n, 0.5, 0.6, 0.4, &[true], &[false]).unwrap();
        assert!(out.is_none());
        assert_eq!(n, 0);
    }

    #[test]
    #[should_panic(expected = "human: merge")]
    fn maybe_merge_hits_reserved_merge_when_enabled() {
        let cfg = GepaConfig {
            merge: true,
            ..GepaConfig::default()
        };
        let mut n = 0;
        let _ = maybe_merge(&cfg, &mut n, 0.5, 0.6, 0.4, &[true, false], &[false, true]);
    }

    /// Manual debug while implementing Algorithm 1.
    #[test]
    #[ignore = "manual debug harness — run with --ignored while implementing evolve"]
    fn manual_evolve() {
        let seed = KeywordSystem::new("seed");
        let feedback = vec!["a".into(), "b".into(), "c".into()];
        let pareto = vec!["a".into(), "b".into()];
        match evolve(seed, &feedback, &pareto, &GepaConfig::default()) {
            Ok(result) => println!(
                "best={} rollouts={}/{}",
                result.best_index, result.state.rollouts.feedback, result.state.rollouts.pareto
            ),
            Err(e) => println!("error: {e}"),
        }
    }
}
