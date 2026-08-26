//! Stage 0 calibration tasks (AGE-23).
//!
//! Tiny known-answer tasks that prove each mechanism fires before Stage B.
//! Decision rule (calibration pass + Stage B fail → escalate model / drop claim)
//! is **human-reserved** — agents only build, run, and record numbers.

use serde::{Deserialize, Serialize};

/// Which module a calibration task belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CalibrationModule {
    Gepa,
    Ace,
    Aflow,
    React,
    Dgm,
}

/// Result of one calibration run (for Marcel to apply the decision rule).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CalibrationResult {
    pub module: CalibrationModule,
    pub passed: bool,
    pub detail: String,
    pub estimated_usd: f64,
    pub elapsed_secs: f64,
}

/// Runners return [`CalibrationResult`]. Bodies fill in as Stage A mechanisms land.
pub trait CalibrationTask {
    fn module(&self) -> CalibrationModule;
    fn run(&self) -> CalibrationResult;
}

/// GEPA: synthetic task whose optimal instruction is known.
pub struct GepaCalibration;

impl CalibrationTask for GepaCalibration {
    fn module(&self) -> CalibrationModule {
        CalibrationModule::Gepa
    }

    fn run(&self) -> CalibrationResult {
        CalibrationResult {
            module: self.module(),
            passed: false,
            detail: "blocked: needs chatty-optimize select_candidate + FeedbackFn (AGE-15/AGE-5)"
                .into(),
            estimated_usd: 0.0,
            elapsed_secs: 0.0,
        }
    }
}

/// ACE: ten episodes requiring five specific facts in the playbook.
pub struct AceCalibration;

impl CalibrationTask for AceCalibration {
    fn module(&self) -> CalibrationModule {
        CalibrationModule::Ace
    }

    fn run(&self) -> CalibrationResult {
        CalibrationResult {
            module: self.module(),
            passed: false,
            detail: "blocked: needs chatty-playbook apply + grow_and_refine (AGE-17)".into(),
            estimated_usd: 0.0,
            elapsed_secs: 0.0,
        }
    }
}

/// AFlow: known-optimal Generate → Ensemble(3) topology.
pub struct AflowCalibration;

impl CalibrationTask for AflowCalibration {
    fn module(&self) -> CalibrationModule {
        CalibrationModule::Aflow
    }

    fn run(&self) -> CalibrationResult {
        CalibrationResult {
            module: self.module(),
            passed: false,
            detail: "blocked: needs soft_mixed_select + WorkflowRepr (AGE-13)".into(),
            estimated_usd: 0.0,
            elapsed_secs: 0.0,
        }
    }
}

/// ReAct: two-hop scripted env, ≥2 searches + correct finish.
pub struct ReactCalibration;

impl CalibrationTask for ReactCalibration {
    fn module(&self) -> CalibrationModule {
        CalibrationModule::React
    }

    fn run(&self) -> CalibrationResult {
        // Scripted WikiEnv path can already assert shape without live LLM.
        use crate::wiki_env::{WikiEnv, WikiEnvConfig};
        let mut env = WikiEnv::new(WikiEnvConfig {
            max_searches: 4,
            scripted: true,
        });
        env.search("hop1");
        env.search("hop2");
        let done = env.finish("answer");
        let passed = env.search_count() >= 2 && done.done;
        CalibrationResult {
            module: self.module(),
            passed,
            detail: if passed {
                "scripted WikiEnv: ≥2 searches and finish (LLM loop calibration still pending)"
                    .into()
            } else {
                "scripted WikiEnv failed basic shape".into()
            },
            estimated_usd: 0.0,
            elapsed_secs: 0.0,
        }
    }
}

/// DGM: trivial method in a throwaway scratch repo — lives in agenticloop, not here.
pub struct DgmCalibration;

impl CalibrationTask for DgmCalibration {
    fn module(&self) -> CalibrationModule {
        CalibrationModule::Dgm
    }

    fn run(&self) -> CalibrationResult {
        CalibrationResult {
            module: self.module(),
            passed: false,
            detail:
                "out of repo: run in agenticloop against a throwaway scratch repo (never chatty2)"
                    .into(),
            estimated_usd: 0.0,
            elapsed_secs: 0.0,
        }
    }
}

/// Run all five calibration tasks and return results for human review.
pub fn run_all_calibrations() -> Vec<CalibrationResult> {
    vec![
        GepaCalibration.run(),
        AceCalibration.run(),
        AflowCalibration.run(),
        ReactCalibration.run(),
        DgmCalibration.run(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_five_modules_represented() {
        let results = run_all_calibrations();
        assert_eq!(results.len(), 5);
        assert!(results.iter().any(|r| r.module == CalibrationModule::React));
        let react = results
            .iter()
            .find(|r| r.module == CalibrationModule::React)
            .unwrap();
        assert!(react.passed);
    }
}
