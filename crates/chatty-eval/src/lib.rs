//! Dev-only helpers for the Self-improving chatty2 research project (AGE-24).
//!
//! **Stage B sandboxes live in `harbor-chatty` (Harbor), not here.** This crate keeps
//! what Harbor cannot replace: paired statistics, ablation flags, QA loaders for
//! *in-process* optimizers, and Stage 0 calibration stubs.
//!
//! **Must never be a dependency of shipping crates** (`chatty-core`, `chatty-trace`,
//! `chatty-playbook`, `chatty-flow`). CI enforces isolation via `cargo tree`.
//!
//! # Deferred: FeedbackFn scorers
//!
//! Natural-language `FeedbackFn` scorers wait on `chatty-trace` contracts from AGE-5 /
//! the AGE-22 walking skeleton.

#![forbid(unsafe_code)]

pub mod ablation;
pub mod calibration;
pub mod cost;
pub mod datasets;
pub mod stats;
pub mod strategy;
pub mod wiki_env;

pub use ablation::AblationConfig;
pub use calibration::{
    AceCalibration, AflowCalibration, CalibrationModule, CalibrationResult, CalibrationTask,
    DgmCalibration, GepaCalibration, ReactCalibration, run_all_calibrations,
};
pub use cost::{CostRow, CostRowInput, format_cost_sheet, row_from_model_prices};
pub use datasets::{
    DatasetError, DatasetItem, FeverItem, Gsm8kItem, HotpotQaItem, HumanEvalItem, Split,
    SplitParts, SplitSpec, load_fever, load_gsm8k, load_hotpotqa, load_humaneval, split_items,
};
pub use stats::{
    BinaryOutcome, ContinuousOutcome, McNemarResult, MinimumDetectableEffect,
    PairedBootstrapResult, mcnemar, minimum_detectable_effect_binary, paired_bootstrap_mean_diff,
};
pub use strategy::{Regime, Strategy, StrategyError};
