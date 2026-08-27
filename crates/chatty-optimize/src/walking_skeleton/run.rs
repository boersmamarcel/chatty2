//! GEPA loop glue for the walking skeleton (AGE-22).
//!
//! Uses greedy [`SelectBestCandidate`](crate::gepa::SelectBestCandidate) — three iterations,
//! not full Pareto search. Calls reserved reflection / trace paths when reached.

use chatty_trace::{FeedbackFn, Outcome};

use crate::OptimizeError;
use crate::compound::CompoundSystem;
use crate::datasets::HotpotQaItem;
use crate::gepa::{SelectBestCandidate, prompts::reflection_meta_prompt};
use crate::walking_skeleton::cache::{CacheKey, ResponseCache};
use crate::walking_skeleton::hotpotqa::normalized_exact_match;

/// Defaults from AGE-22 scope.
pub const DEFAULT_ITEM_COUNT: usize = 20;
pub const DEFAULT_GEPA_ITERATIONS: usize = 3;

/// Configuration for one skeleton run.
#[derive(Debug, Clone)]
pub struct WalkingSkeletonConfig {
    pub seed: u64,
    pub model_id: String,
    pub initial_preamble: String,
    pub max_items: usize,
    pub gepa_iterations: usize,
}

impl Default for WalkingSkeletonConfig {
    fn default() -> Self {
        Self {
            seed: 0,
            model_id: "walking-skeleton".into(),
            initial_preamble: String::new(),
            max_items: DEFAULT_ITEM_COUNT,
            gepa_iterations: DEFAULT_GEPA_ITERATIONS,
        }
    }
}

/// Outcome of a skeleton run (prompt history + rollout accounting).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalkingSkeletonResult {
    pub final_preamble: String,
    pub preamble_history: Vec<String>,
    /// Rollouts attempted (train-side minibatch evals in full GEPA; counted per item here).
    pub train_rollouts: u32,
    /// Full pareto-style evals (reserved for acceptance gate; 0 until wired).
    pub pareto_rollouts: u32,
}

/// Run the skeleton: load items, iterate GEPA rounds, stop when reserved paths are hit.
///
/// Returns `NotReady` until Marcel implements `Trajectory`, `FeedbackFn`, and
/// `reflection_meta_prompt`. The harness structure is real; reserved symbols are not.
pub fn run_walking_skeleton(
    config: &WalkingSkeletonConfig,
    items: &[HotpotQaItem],
    system: &dyn CompoundSystem,
    cache: &ResponseCache,
    feedback: &dyn FeedbackFn,
) -> Result<WalkingSkeletonResult, OptimizeError> {
    if items.is_empty() {
        return Err(OptimizeError::NotReady("no HotpotQA items"));
    }
    if config.gepa_iterations == 0 {
        return Err(OptimizeError::InvalidInput(
            "gepa_iterations must be >= 1".into(),
        ));
    }

    let selector = SelectBestCandidate;
    let mut preamble = config.initial_preamble.clone();
    let mut preamble_history = vec![preamble.clone()];
    let mut train_rollouts = 0u32;

    for iter in 0..config.gepa_iterations {
        let mut scores = Vec::with_capacity(items.len());

        for item in items {
            let _cache_key = CacheKey {
                seed: config.seed,
                model_id: config.model_id.clone(),
                preamble: preamble.clone(),
                question_id: item.id.clone(),
            };

            // Rollout + trace capture — reserved `Trajectory` path.
            let trajectory = system.rollout(&preamble, &item.id, &item.question)?;
            train_rollouts += 1;

            // Cache layer is wired; population happens once live LLM loop writes responses.
            let _cached = cache.get(&_cache_key)?;

            // Feedback for reflection — reserved `FeedbackFn` path.
            let outcome = Outcome::from_trajectory(&trajectory)
                .map_err(|e| OptimizeError::InvalidInput(e.to_string()))?;
            let _feedback_text = feedback
                .evaluate(&trajectory, &outcome)
                .map_err(|e| OptimizeError::InvalidInput(e.to_string()))?;

            // Scalar gate for greedy selection (EM on answer text — placeholder until rollout returns text).
            let score = if normalized_exact_match("", &item.answer) {
                1.0
            } else {
                0.0
            };
            scores.push(score);
        }

        let _parent_idx = selector.select(&scores)?;

        // Reflective mutation meta-prompt — reserved.
        let _meta = reflection_meta_prompt();

        // Prompt update placeholder: append marker so iterations are observable once reserved paths land.
        preamble = format!("{} [iter {}]", preamble.trim(), iter + 1);
        preamble_history.push(preamble.clone());
    }

    Ok(WalkingSkeletonResult {
        final_preamble: preamble,
        preamble_history,
        train_rollouts,
        pareto_rollouts: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compound::UnwiredCompoundSystem;
    use chatty_trace::UnimplementedFeedback;
    use tempfile::tempdir;

    fn sample_items() -> Vec<HotpotQaItem> {
        vec![HotpotQaItem {
            id: "hp-1".into(),
            question: "q".into(),
            answer: "a".into(),
            supporting_titles: vec!["Doc".into()],
        }]
    }

    #[test]
    #[should_panic(expected = "human: Trajectory")]
    fn run_stops_at_reserved_trajectory() {
        let dir = tempdir().unwrap();
        let cache = ResponseCache::new(dir.path()).unwrap();
        let config = WalkingSkeletonConfig {
            gepa_iterations: 1,
            ..Default::default()
        };
        let system = UnwiredCompoundSystem::new(vec!["preamble"]);
        let feedback = UnimplementedFeedback;
        let _ = run_walking_skeleton(&config, &sample_items(), &system, &cache, &feedback);
    }
}
