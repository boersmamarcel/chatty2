//! Ablation switches reachable from config (AGE-24).
//!
//! Wiring into optimizers happens in Stage A/B; this module only defines the flags.

use serde::{Deserialize, Serialize};

/// Named ablations from the module issues. Default = paper-faithful (all features on).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AblationConfig {
    /// M3 GEPA: replace Pareto `SelectCandidate` with always-pick-best.
    pub select_best_candidate: bool,
    /// M2 AFlow: disable operator library (blank-template / free-form only).
    pub no_operator: bool,
    /// M4 ACE: skip Reflector role.
    pub no_reflector: bool,
    /// M4 ACE: single epoch only.
    pub no_multi_epoch: bool,
    /// M4 ACE: skip warmup.
    pub no_warmup: bool,
    /// M4 ACE: monolithic LLM rewrite control (negative control).
    pub monolithic_rewrite: bool,
    /// M5 DGM: disable archive (hill-climb only). Lives in agenticloop.
    pub no_archive: bool,
    /// M5 DGM: disable self-improvement proposals.
    pub no_self_improve: bool,
}

impl AblationConfig {
    /// Paper-faithful defaults (all ablations off).
    pub fn paper_faithful() -> Self {
        Self::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_json() {
        let mut cfg = AblationConfig::paper_faithful();
        cfg.select_best_candidate = true;
        cfg.no_operator = true;
        let json = serde_json::to_string(&cfg).unwrap();
        let back: AblationConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(cfg, back);
    }
}
