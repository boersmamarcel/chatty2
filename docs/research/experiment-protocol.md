# Experiment protocol

**When to read this:** You are running or reviewing Stage A/B experiments and need the
shared bar for when results count toward a promotion decision.

Human interpretation: [AGE-21](https://linear.app/agents-research/issue/AGE-21) (Marcel only).
Agents: build harnesses and cost sheets; do not write up cross-module conclusions.

## Two stages

| Stage | Location | Pass criterion |
|-------|----------|----------------|
| **A — Fidelity** | `chatty2` research crates | Mechanism matches paper; acceptance criteria in module page checked |
| **B — Task value** | [`harbor-chatty`](../../../harbor-chatty) | Gain on representative tasks under paired stats; cost within budget |

Neither stage auto-promotes. See [promotion log](./promotion-log.md).

## Stage A checklist (per module)

1. Read the [module page](./modules/index.md) acceptance criteria.
2. Run with **fixed seed** + **warm response cache** where determinism is claimed.
3. Separate **training** vs **validation** rollouts (critical for M3 GEPA).
4. Log artifact token counts when comparing ACE vs GEPA (M4 vs M3).
5. File failures as Linear issues — do not patch reserved symbols.

## Stage B checklist

1. Use Harbor adapter ([AGE-34](https://linear.app/agents-research/issue/AGE-34)) — not in-repo sandboxes.
2. Report paired differences with confidence, not single-run deltas.
3. Use [cost model](./cost-model.md) with live `ModelConfig` prices — never hand-typed dollars.
4. Marcel posts authoritative numbers before they appear in docs or promotion log.

## Cost accounting

```
cost ≈ rollouts × calls_per_rollout × (in_tok×in_price + out_tok×out_price) / 1e6
       × (1 − cache_hit_rate) × seeds
```

Implementation: `chatty_optimize::cost`. Token means blocked on M0 rig-tap wiring (AGE-5).

Until pilot means exist, **do not treat Stage B dollar figures as authoritative**.

## Determinism requirements

| Claim | Requirement |
|-------|-------------|
| Byte-identical traces | Same seed + warm cache |
| Reproducible workflow score | Serialize → reload → re-execute |
| Stable playbook serialization | No-op update must not reorder sections (M4) |

## Cross-module experiments (AGE-21)

Reserved whole for Marcel. Examples:

- ACE vs GEPA rollout counts on same task
- Combined workflow + playbook + preamble
- Ordering across modules no single paper reports

Agents may build plumbing; Marcel runs and interprets.

## Promotion decision inputs

After Stage A + B, Marcel records in [promotion log](./promotion-log.md):

1. **Effect size** — paired stats, MDE met?
2. **Cost / latency** — acceptable on desktop agent?
3. **Production bar** — [AGE-26](https://linear.app/agents-research/issue/AGE-26) satisfied for shipping crates?
4. **Product gates** — [AGE-44–47](https://linear.app/agents-research/issue/AGE-37) answered?
5. **Verdict** — `rejected` / `setting` / `default`

## Related

- [Paper → product pipeline](./paper-to-product-pipeline.md)
- [Harbor pivot](./harbor-pivot.md)
- [cost-model.md](./cost-model.md)
