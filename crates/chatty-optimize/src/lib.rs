//! Optimizers and optimizer-side helpers (AGE-7 / AGE-8 / AGE-15 / AGE-23 / AGE-25).
//!
//! Invoked on demand or in CI — **never on a request path**. Stage B sandboxes
//! (HumanEval, Polyglot, AppWorld, …) live in sibling `harbor-chatty` (Harbor / AGE-34),
//! not here.
//!
//! This crate also holds what used to be the thin `chatty-eval` remainder:
//! paired statistics, ablation flags, QA loaders for in-process GEPA/ACE, calibration
//! stubs, and the reserved ReAct `Strategy`.
//!
//! # Reserved
//!
//! `SelectionStrategy`, `soft_mixed_select`, `select_candidate`, `merge`,
//! `REFLECTION_META_PROMPT`, and `Strategy` are human-reserved
//! ([`RESERVED.md`](../../../RESERVED.md)).

#![forbid(unsafe_code)]

pub mod ablation;
pub mod aflow;
pub mod archive;
pub mod calibration;
pub mod cost;
pub mod datasets;
pub mod gepa;
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
    PairedBootstrapResult, format_paired_binary_report, mcnemar, minimum_detectable_effect_binary,
    paired_bootstrap_mean_diff,
};
pub use strategy::{Regime, Strategy, StrategyError};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum OptimizeError {
    #[error("optimize not yet available: {0}")]
    NotReady(&'static str),
    #[error("invalid selection input: {0}")]
    InvalidInput(String),
}

pub fn crate_ready() -> Result<(), OptimizeError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiles() {
        crate_ready().unwrap();
    }
}
