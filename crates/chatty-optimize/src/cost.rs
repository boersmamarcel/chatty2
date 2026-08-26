//! Cost model sheet scaffolding (AGE-25).
//!
//! Unit prices **must** come from `ModelConfig.cost_per_million_*` — never hand-entered.
//! Pilot token measurement via `rig-tap` waits on AGE-5 / the AGE-22 walking skeleton.

use serde::{Deserialize, Serialize};

/// One row of the Stage B cost sheet.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CostRow {
    pub module: String,
    pub benchmark: String,
    pub rollouts: u64,
    pub calls_per_rollout: f64,
    pub mean_input_tokens: f64,
    pub mean_output_tokens: f64,
    pub cache_hit_rate: f64,
    pub model_id: String,
    /// USD per million input tokens — from ModelConfig.
    pub cost_per_million_input: f64,
    /// USD per million output tokens — from ModelConfig.
    pub cost_per_million_output: f64,
    pub seeds: u32,
}

impl CostRow {
    /// `cost ≈ rollouts × calls × (in_price×in_tok + out_price×out_tok)/1e6 × (1 − cache) × seeds`
    pub fn estimated_usd(&self) -> f64 {
        let calls = self.rollouts as f64 * self.calls_per_rollout;
        let input_cost = calls * self.mean_input_tokens * self.cost_per_million_input / 1_000_000.0;
        let output_cost =
            calls * self.mean_output_tokens * self.cost_per_million_output / 1_000_000.0;
        let uncached = (1.0 - self.cache_hit_rate.clamp(0.0, 1.0)).max(0.0);
        (input_cost + output_cost) * uncached * self.seeds as f64
    }
}

/// Inputs for [`CostRow`] construction from ModelConfig-style prices.
#[derive(Debug, Clone)]
pub struct CostRowInput {
    pub module: String,
    pub benchmark: String,
    pub model_id: String,
    pub cost_per_million_input: f64,
    pub cost_per_million_output: f64,
    pub rollouts: u64,
    pub calls_per_rollout: f64,
    pub mean_input_tokens: f64,
    pub mean_output_tokens: f64,
    pub cache_hit_rate: f64,
    pub seeds: u32,
}

/// Build a cost row from ModelConfig-style prices (pass the fields explicitly so
/// chatty-optimize does not depend on chatty-core).
pub fn row_from_model_prices(input: CostRowInput) -> CostRow {
    CostRow {
        module: input.module,
        benchmark: input.benchmark,
        rollouts: input.rollouts,
        calls_per_rollout: input.calls_per_rollout,
        mean_input_tokens: input.mean_input_tokens,
        mean_output_tokens: input.mean_output_tokens,
        cache_hit_rate: input.cache_hit_rate,
        model_id: input.model_id,
        cost_per_million_input: input.cost_per_million_input,
        cost_per_million_output: input.cost_per_million_output,
        seeds: input.seeds,
    }
}

/// Markdown table for the cost sheet (one row per Stage B).
pub fn format_cost_sheet(rows: &[CostRow]) -> String {
    let mut out = String::from(
        "| Module | Benchmark | Model | Rollouts | Calls/rollout | Est. USD (×seeds) |\n\
         | -- | -- | -- | -- | -- | -- |\n",
    );
    for r in rows {
        out.push_str(&format!(
            "| {} | {} | {} | {} | {:.1} | ${:.2} |\n",
            r.module,
            r.benchmark,
            r.model_id,
            r.rollouts,
            r.calls_per_rollout,
            r.estimated_usd()
        ));
    }
    out.push_str(
        "\n> Token means marked **pilot-pending** until rig-tap pilots land (AGE-5 / AGE-22).\n\
         > Prices must be copied from `ModelConfig.cost_per_million_input_tokens` / \
         `cost_per_million_output_tokens`.\n",
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geapa_hotpot_example_scales() {
        // Paper-shaped illustration with placeholder prices ($1 / $3 per MTok).
        let row = row_from_model_prices(CostRowInput {
            module: "M3 GEPA".into(),
            benchmark: "HotpotQA".into(),
            model_id: "gpt-4.1-mini".into(),
            cost_per_million_input: 1.0,
            cost_per_million_output: 3.0,
            rollouts: 6438,
            calls_per_rollout: 5.0,
            mean_input_tokens: 2000.0,
            mean_output_tokens: 400.0,
            cache_hit_rate: 0.5,
            seeds: 3,
        });
        let usd = row.estimated_usd();
        assert!(usd > 0.0);
        // 6438*5 calls * (2000*$1 + 400*$3)/1e6 * 0.5 * 3
        let expected = 6438.0 * 5.0 * (2000.0 * 1.0 + 400.0 * 3.0) / 1e6 * 0.5 * 3.0;
        assert!((usd - expected).abs() < 1e-6);
    }

    #[test]
    fn sheet_mentions_pilot_pending() {
        let sheet = format_cost_sheet(&[]);
        assert!(sheet.contains("pilot-pending"));
    }
}
