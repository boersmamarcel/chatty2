# M2 — AFlow workflow topology search

**Paper:** Zhang, Xiang, Yu, Teng, Chen, Chen, Zhuge et al. — *AFlow: Automating Agentic
Workflow Generation* ([ICLR 2025 Oral](https://doi.org/10.48550/arXiv.2410.10762))

**Linear:** [AGE-7](https://linear.app/agents-research/issue/AGE-7) · **Crates:**
`chatty-flow` (runtime IR + interpreter), `chatty-optimize` (MCTS search) ·
**Promotion:** pending

## Mechanism

Search over **workflow topology** before optimizing prompt text:

- Nodes = LLM calls `(model, prompt, temperature, format)`; edges = control flow
- **Operators:** Generate, Format, Review & Revise, Ensemble, Test, Programmer, Custom
- **MCTS:** each tree node is a complete workflow; soft-mixed selection (λ=0.2, α=0.4);
  LLM expansion with experience log; 5× validation evaluation; backpropagate diffs

Kill criterion: if `IrRepr` cannot express the search space by week 6, wait for Monty code
mode or narrow the search — **no third scripting language**.

## Chatty mapping

| Paper concept | Chatty location | Status |
|---------------|-----------------|--------|
| Workflow representation | `WorkflowRepr` trait | Reserved (`WfNode`) |
| IR interpreter | `chatty-flow` → sub-agent / tool registry | To build |
| MCTS search | `chatty-optimize` (offline only) | To build |
| Composition substrate | `sub_agent_tool`, `invoke_agent_tool`, `list_agents_tool` | **Ships** |
| Monty code mode (future) | `sandbox/monty_bridge.rs` | Interface only; VM not wired |
| Saved workflow | Future `FlowSettingsModel` | Gate: product integration |

```mermaid
flowchart LR
  Search[chatty-optimize MCTS] --> IR[WorkflowRepr JSON]
  IR --> Interpreter[chatty-flow interpreter]
  Interpreter --> Tools[Existing tool registry]
  Tools --> Core[chatty-core agents]
```

**Split is intentional:** search never ships in the app binary; interpreter does.

## Eval protocol

**Stage A:**

- All 7 operators as composable `Op`s
- Selection matches closed form for λ=0.2, α=0.4; blank template retains mass every round
- Experience log stores parent **diff**, not just score
- Discovered workflow serializes, reloads, re-executes to same score under warm cache

**Stage B:** HotpotQA / DROP / HumanEval / MBPP / GSM8K / MATH — target ordering (+5.7%
over best manual design reported in paper; do not treat as binding until Marcel posts numbers).

## Production landing

| Mechanism | Likely promotion |
|-----------|------------------|
| MCTS search | Offline / CI only — **reject** from request path |
| `IrRepr` interpreter | **Setting** (`workflow_enabled`, `active_workflow_id`) |
| Winning workflow JSON | User-selected artifact via apply policy ([AGE-45](https://linear.app/agents-research/issue/AGE-45)) |

## Reserved symbols

`WorkflowRepr`, `WfNode`, `soft_mixed_select`, `SelectionStrategy` — see
[`RESERVED.md`](../../../RESERVED.md).

## Depends on

- [M0 Trace](./m0-trace.md), [M1 ReAct](./m1-react.md)
- [AGE-30](https://linear.app/agents-research/issue/AGE-30) reflection gate (representation decision)

## Further reading

- [crate-promises-chatty-flow](../crate-promises-chatty-flow.md)
- [Monty sandbox](../../monty-sandbox.md)
