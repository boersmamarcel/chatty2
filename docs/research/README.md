# Research notes

**When to read this:** You want to know what the research crates in this workspace are
for, how they relate to the product, and where the decisions behind them are recorded.

Chatty ships a working agent: a ReAct-shaped tool loop, memory and skills, a token
budget with compaction, ATIF trace export. The research track takes a handful of
agentic-self-improvement papers and asks, per mechanism, whether it earns a place in that
product — as a default, as an opt-in setting, or not at all. Papers inform the design;
experiments decide what ships. The full frame is
[Paper → experiment → product](./paper-to-product-pipeline.md).

Work is tracked in the Linear project
**[Self-improving chatty2](https://linear.app/agents-research/project/self-improving-chatty2)**.
A small set of symbols in the research crates is reserved for the human to write; see
[`RESERVED.md`](../../RESERVED.md) before touching those crates.

## The modules

| Module | Paper | Lands in | What it adds |
|--------|-------|----------|--------------|
| [M0 Trace](./modules/m0-trace.md) | — (contract layer) | [`chatty-trace`](../../crates/chatty-trace/README.md) | The `Trajectory` every optimizer reflects on; per-step attribution; ATIF round-trip; `FeedbackFn` |
| [M1 ReAct](./modules/m1-react.md) | Yao et al., ICLR 2023 | `chatty-core` (loop already ships) + `chatty-trace` | Fidelity to the paper's action space; strategy variants for eval |
| [M2 AFlow](./modules/m2-aflow.md) | Zhang et al., ICLR 2025 | [`chatty-flow`](../../crates/chatty-flow/README.md) (IR + interpreter), [`chatty-optimize`](../../crates/chatty-optimize/README.md) (MCTS) | Workflow topology search offline; saved workflows run in-app |
| [M3 GEPA](./modules/m3-gepa.md) | Agrawal et al., ICLR 2026 | [`chatty-optimize`](../../crates/chatty-optimize/README.md) | Reflective prompt optimization of `ModelConfig.preamble` |
| [M4 ACE](./modules/m4-ace.md) | Zhang et al., ICLR 2026 | [`chatty-playbook`](../../crates/chatty-playbook/README.md) | Evolving playbook over the memory/skills store with deterministic merge |

Per-module pages, dependency graph and status: [modules/index.md](./modules/index.md).
The fifth paper (DGM) self-modifies its target repo and therefore lives in a separate
repo, never against chatty2.

## How research meets the product

- **Stage A** (fidelity) runs inside this workspace's research crates. **Stage B** (task
  value) runs in the sibling repo
  [`harbor-chatty`](https://github.com/boersmamarcel/harbor-chatty) — see
  [Harbor pivot](./harbor-pivot.md) for why. Neither stage promotes anything by itself;
  the bar is in [Experiment protocol](./experiment-protocol.md).
- **Shipping crates** (`chatty-trace`, `chatty-playbook`, `chatty-flow`) sit on the
  request path and are held to the same production bar as `chatty-core`; `chatty-optimize`
  is build-time tooling and never blocks a chat.
- **Where mechanisms land**: [App ↔ research bridge](./app-research-bridge.md) maps
  production components (agent loop, memory, context window, traces, sub-agents) to the
  modules that touch them; [Settings integration map](./settings-integration-map.md)
  lists the settings each promoted mechanism would surface through.

## Decision records (ADRs)

| Doc | Decision |
|-----|----------|
| [harbor-pivot.md](./harbor-pivot.md) | Stage B evaluation lives in Harbor, not in chatty2 |
| [appworld-decision.md](./appworld-decision.md) | Which AppWorld sandbox to evaluate against |
| [cost-model.md](./cost-model.md) | How optimizer runs are costed from live `ModelConfig` prices |

Each research crate's scope is stated in its own `README.md` (linked in the table above).

**Archived:** the empty promotion-log template and the pre-Stage-A crate-promise pages
are kept under [`docs/archive/research/`](../archive/research/promotion-log.md) for their
reasoning only; they are not maintained.
