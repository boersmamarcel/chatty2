# M3 — GEPA reflective prompt evolution

**Paper:** Agrawal, Tan, Soylu, Ziems, Khare, Opsahl-Ong, Singhvi, … Zaharia, Khattab —
*GEPA: Reflective Prompt Evolution Can Outperform Reinforcement Learning*
([ICLR 2026 Oral](https://doi.org/10.48550/arXiv.2507.19457))

**Linear:** [AGE-8](https://linear.app/agents-research/issue/AGE-8) · **Crate:**
`chatty-optimize` (offline) · **Promotion:** pending

## Mechanism

Optimize prompts **Π** in a compound system Φ = (modules, control flow) under rollout
budget **B** — weights frozen, no training stack required.

Three load-bearing details:

1. **Pareto candidate selection** — keep every candidate best on ≥1 instance; prune
   dominated; sample proportional to win count. Ablating to greedy costs **6.4 aggregate
   points** in the paper.
2. **`FeedbackFn` (µ_f)** — scalar plus natural-language traces (compiler output, missing
   docs, failed constraints), optionally per module. Scalar-only = prompt shuffling.
3. **Reflective mutation** — round-robin module updates; meta-prompt asks for domain-specific
   declarative instructions (not quasi-exemplars).

Optional **Merge** (`GEPA+Merge`): system-aware crossover, capped at 5 invocations, **off by
default** (hurts some models).

## Chatty mapping

| Paper concept | Chatty location | Status |
|---------------|-----------------|--------|
| Module prompts | `ModelConfig.preamble` | **Ships** (user-editable) |
| Compound system | ReAct loop (M1) or `IrRepr` (M2) | M1 first; M2 later |
| Pareto archive | `chatty-optimize` | To build |
| `select_candidate` | `chatty-optimize/src/gepa/select.rs` | Reserved |
| `merge` | `chatty-optimize/src/gepa/merge.rs` | Reserved |
| `REFLECTION_META_PROMPT` | `chatty-optimize/src/gepa/prompts.rs` | Reserved |
| Train vs validation rollouts | `chatty-trace` budget view | To build |
| Per-model cost | `ModelConfig.cost_per_million_*` | Ships |

**Product feature:** "optimize this agent's preamble against a task set you supply" — never
in the chat request path.

## Eval protocol

**Stage 0 (AGE-23):** synthetic task with known-optimal instruction recovered by one
reflective mutation — separates broken code from broken reflection.

**Stage A:**

- Pareto selection unit-tested against paper's worked example
- Module-level `µ_f` for multi-hop system
- Rollout accounting splits train vs validation
- Same optimizer runs against M1 system and M2 `IrRepr` (required before module done)

**Stage B:** HotpotQA, IFBench, HoVer, PUPA — Marcel posts numbers before agents cite them.

## Production landing

| Mechanism | Likely promotion |
|-----------|------------------|
| Full GEPA search | Offline / `chatty-tui --optimize` / CI ([AGE-44](https://linear.app/agents-research/issue/AGE-44)) |
| Optimized preamble | **Setting** per model |
| Apply optimized preamble | Review UI / export / auto ([AGE-45](https://linear.app/agents-research/issue/AGE-45)) |
| Merge operator | Ablation flag, default off |

## Reserved symbols

`select_candidate`, `merge`, `REFLECTION_META_PROMPT` — see [`RESERVED.md`](../../../RESERVED.md).

## Depends on

- [M0 Trace](./m0-trace.md) — `FeedbackFn`, budget split
- [M1 ReAct](./m1-react.md) — first `CompoundSystem` (not blocked on M2)

## Antagonist

[M4 ACE](./m4-ace.md) argues against brevity-bias optimizers like GEPA — head-to-head in
[AGE-21](https://linear.app/agents-research/issue/AGE-21) only.

## Further reading

- [`cost-model.md`](../cost-model.md)
