# ADRs & research decisions

**When to read this:** You need context on research architecture, crate boundaries, or Stage B eval placement.

> **Pair review pending (DOC-13 / AGE-95):** These pages were migrated from `docs/research/` into the mdBook site. Marcel reviews tone and accuracy before treating them as final ADRs. Do not close this issue until review is complete.

## Linear project

All research crates and experiment protocol live under the **[Self-improving chatty2](https://linear.app/agents-research/project/self-improving-chatty2)** Linear project.

Stage B sandboxes (HumanEval, Polyglot, AppWorld, …) are in the sibling repo [`harbor-chatty`](https://github.com/boersmamarcel/harbor-chatty) — see [Harbor pivot](./harbor-pivot.md).

## Decision records

| Doc | Topic |
|-----|-------|
| [harbor-pivot.md](./harbor-pivot.md) | Stage B eval lives in Harbor, not chatty2 |
| [crate-promises-chatty-trace.md](./crate-promises-chatty-trace.md) | chatty-trace scope and boundaries |
| [crate-promises-chatty-playbook.md](./crate-promises-chatty-playbook.md) | chatty-playbook scope and boundaries |
| [crate-promises-chatty-flow.md](./crate-promises-chatty-flow.md) | chatty-flow scope and boundaries |
| [cost-model.md](./cost-model.md) | Optimizer economics |
| [appworld-decision.md](./appworld-decision.md) | AppWorld eval sandbox choice |

## Pipeline & integration

| Doc | Topic |
|-----|-------|
| [app-research-bridge.md](./app-research-bridge.md) | Production app ↔ M0–M4 module map |
| [paper-to-product-pipeline.md](./paper-to-product-pipeline.md) | SOTA → experiment → product spine |
| [experiment-protocol.md](./experiment-protocol.md) | Stage A/B checklist, cost accounting |
| [promotion-log.md](./promotion-log.md) | Marcel-only promotion verdicts |
| [settings-integration-map.md](./settings-integration-map.md) | Settings ↔ research mechanisms |

Module work breakdown: [modules/index.md](./modules/index.md) (M0–M4).
