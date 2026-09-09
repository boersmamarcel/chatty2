# Promotion log

**Who updates this:** Marcel only. Agents link ADRs and module pages here but **do not**
fill verdicts or cite benchmark numbers before Marcel posts them.

Append one row when a mechanism completes experimentation and you decide how it ships.

## Status values

| Promotion | Meaning |
|-----------|---------|
| `pending` | Still in Stage A/B or awaiting product gate |
| `rejected` | Stays research/offline; not exposed in app |
| `setting` | User-configurable; off by default |
| `default` | Normal path when evidence shows dominance |

## Log

| Date | Mechanism | Module | Stage completed | Promotion | Evidence / ADR | Settings surface |
|------|-----------|--------|-----------------|-----------|----------------|------------------|
| — | ReAct agent loop | M1 | pre-research | `default` | Already ships in `chatty-core` | — |
| — | CoT / CoT-SC / Act strategies | M1 | — | `pending` | — | TBD |
| — | Per-module trace attribution | M0 | — | `pending` | — | `TrainingSettingsModel.atif_auto_export` |
| — | Full trace retention | M0 | — | `pending` | — | Recorder opt-in |
| — | AFlow MCTS search | M2 | — | `pending` | — | Offline only |
| — | Workflow IR interpreter | M2 | — | `pending` | — | `FlowSettingsModel` (planned) |
| — | GEPA preamble optimization | M3 | — | `pending` | — | `ModelConfig.preamble` |
| — | GEPA Merge crossover | M3 | — | `pending` | — | Ablation flag |
| — | ACE playbook runtime | M4 | — | `pending` | — | [AGE-47](https://linear.app/agents-research/issue/AGE-47) |
| — | ACE grow-and-refine eviction | M4 | — | `pending` | — | Production bounds TBD |

## How to add a row

1. Complete Stage A fidelity + Stage B (if applicable).
2. Answer any blocking product gates ([AGE-44–47](https://linear.app/agents-research/issue/AGE-37)).
3. Append row with date, promotion verdict, link to ADR or experiment write-up.
4. Update the module page **Promotion** field to match.

## Related

- [Paper → product pipeline](./paper-to-product-pipeline.md)
- [Module pages](./modules/index.md)
- [Settings integration map](./settings-integration-map.md)
