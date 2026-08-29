# M1 — ReAct substrate loop

**Paper:** Yao, Zhao, Yu, Du, Shafran, Narasimhan, Cao — *ReAct: Synergizing Reasoning and
Acting in Language Models* ([ICLR 2023](https://doi.org/10.48550/arXiv.2210.03629))

**Linear:** [AGE-6](https://linear.app/agents-research/issue/AGE-6) · **Crates:**
`chatty-core` (loop), `chatty-trace` (attribution) · **Promotion:** loop = **default**;
variants pending

## Mechanism (what the paper actually claims)

Augment action space to **Â = A ∪ L** — language thoughts condition the next action without
changing environment state. Trajectory = `(thought, action, observation)` triples from a
**frozen** LLM with few-shot exemplars.

Two regimes:

- **Knowledge-intensive** (HotpotQA, FEVER): dense thoughts; `search` / `lookup` / `finish`
- **Decision-making** (ALFWorld, WebShop): sparse thoughts; model decides when to think

**Important:** ReAct alone does **not** beat CoT-SC on HotpotQA in the paper — it wins on
FEVER and interaction tasks. Reproducing "ReAct beats CoT on HotpotQA" would be wrong.

## Chatty mapping

| Paper concept | Chatty location | Status |
|---------------|-----------------|--------|
| ReAct loop | `chatty-core` `llm_service.rs` + `agent_factory/` | **Ships today** |
| ~60 tools | `chatty-core/src/tools/` | Ships |
| Wikipedia env | Adapter over `search_tool` / `fetch_tool` | Stage A |
| Strategy variants (Act, CoT, CoT-SC, hybrids) | `chatty-optimize::Strategy` | Reserved backoff rule |
| Per-step trace | [M0 Trace](./m0-trace.md) | WIP |
| Few-shot exemplar prefix | `ModelConfig.preamble` | Ships (user-editable) |

**Do not rebuild the loop.** Extend fidelity and capture only.

## Eval protocol

**Stage A:**

- Real HotpotQA run → `Trajectory` round-trips with per-step `ModuleId`
- Four prompt regimes: dense/sparse × QA/interaction
- Wikipedia `lookup` returns k-th matching sentence (paper semantics)
- Reproduce **ordering** of methods, not absolute PaLM-540B numbers

**Stage B:** Harbor coding/env tasks using the same tool registry.

## Production landing

| Mechanism | Likely promotion |
|-----------|------------------|
| ReAct loop with tools | **Default** (already) |
| CoT / CoT-SC / Act / hybrid strategies | **Setting** or eval-only |
| WikiEnv adapter | Eval harness only |

## Reserved symbols

`Strategy` backoff rule and loop termination semantics — see [`RESERVED.md`](../../../RESERVED.md).

## Depends on

- [M0 Trace](./m0-trace.md)
- [AGE-32](https://linear.app/agents-research/issue/AGE-32) rig upgrade

## Downstream

Everything. M2–M4 all reflect on traces from this loop.
