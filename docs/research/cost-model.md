# AGE-25 Cost model sheet

**Rule:** unit prices come from `ModelConfig.cost_per_million_input_tokens` /
`cost_per_million_output_tokens` in `settings/models/models_store.rs` — never hand-typed
into a Stage B issue.

**Formula:**

```
cost ≈ rollouts × calls_per_rollout × (in_tok×in_price + out_tok×out_price) / 1e6
       × (1 − cache_hit_rate) × seeds
```

## Status

| Field | Status |
| -- | -- |
| Sheet template + estimator | Landed in `chatty-eval::cost` |
| Prices from ModelConfig | API ready (`row_from_model_prices`) — fill per run from live settings |
| Measured token means via `rig-tap` | **Blocked** on AGE-5 / AGE-22 (rig-tap not wired yet) |
| Per-module caps + subset sizing | After pilots; drop claims that cannot meet MDE under budget |

## Placeholder rows (replace means after pilot)

Use `chatty_eval::cost::format_cost_sheet` once prices and pilot means are known.
Until then, do not treat any Stage B dollar figure as authoritative.

## Module 5

Budgeted and capped **separately**. Self-modification step uses the documented frontier-model
exception; M5 calibration (AGE-23) must use that same model.
