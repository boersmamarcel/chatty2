//! AGE-22 walking skeleton — HotpotQA + cache + GEPA glue.
//!
//! **Reserved (human):** `Trajectory`, `Step`, `Action`, `Outcome`, `FeedbackFn`,
//! `select_candidate`, `merge`, `REFLECTION_META_PROMPT`.
//!
//! **Agent-built here:** dataset slice, response cache, greedy GEPA iteration harness,
//! HotpotQA metric helpers, `CompoundSystem` seam.
//!
//! Live LLM rollouts via chatty-core + rig-tap land once the trace contract is written.
//! Record gaps in [`docs/research/walking-skeleton-gap-list.md`](../../../docs/research/walking-skeleton-gap-list.md).

mod cache;
mod hotpotqa;
mod run;

pub use cache::{CacheKey, ResponseCache};
pub use hotpotqa::{missing_supporting_titles, normalized_exact_match, select_items};
pub use run::{
    DEFAULT_GEPA_ITERATIONS, DEFAULT_ITEM_COUNT, WalkingSkeletonConfig, WalkingSkeletonResult,
    run_walking_skeleton,
};

use std::path::Path;

use crate::OptimizeError;
use crate::compound::CompoundSystem;
use crate::datasets::load_hotpotqa;
use chatty_trace::FeedbackFn;

/// Load HotpotQA, select a deterministic slice, and run the skeleton harness.
pub fn run_from_dataset_path(
    config: &WalkingSkeletonConfig,
    dataset_path: impl AsRef<Path>,
    system: &dyn CompoundSystem,
    cache: &ResponseCache,
    feedback: &dyn FeedbackFn,
) -> Result<WalkingSkeletonResult, OptimizeError> {
    let all =
        load_hotpotqa(dataset_path).map_err(|e| OptimizeError::InvalidInput(e.to_string()))?;
    let items = select_items(&all, config.seed, config.max_items);
    run_walking_skeleton(config, &items, system, cache, feedback)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compound::UnwiredCompoundSystem;
    use chatty_trace::UnimplementedFeedback;
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn fixture() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/hotpotqa_sample.jsonl")
    }

    #[test]
    fn select_slice_from_fixture() {
        let all = load_hotpotqa(fixture()).unwrap();
        let slice = select_items(&all, 1, 3);
        assert_eq!(slice.len(), 3);
    }

    #[test]
    #[should_panic(expected = "human: Trajectory")]
    fn run_from_dataset_hits_reserved_trajectory() {
        let dir = tempdir().unwrap();
        let cache = ResponseCache::new(dir.path()).unwrap();
        let config = WalkingSkeletonConfig {
            max_items: 2,
            gepa_iterations: 1,
            ..Default::default()
        };
        let system = UnwiredCompoundSystem::new(vec!["preamble"]);
        let feedback = UnimplementedFeedback;
        let _ = run_from_dataset_path(&config, fixture(), &system, &cache, &feedback);
    }
}
