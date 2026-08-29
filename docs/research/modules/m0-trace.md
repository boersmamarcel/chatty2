# M0 — Trace contract (chatty-trace)

**Linear:** [AGE-5](https://linear.app/agents-research/issue/AGE-5) · **Crate:**
[`chatty-trace`](../../../crates/chatty-trace/) · **Promotion:** pending

No single paper — this is the **contract layer** every optimizer reflects on.

## Why it exists

AFlow, GEPA, ACE, and DGM all optimize against **traces**, not final answers alone.
Chatty already ships a ReAct loop, ATIF v1.6 export, and token budgeting. M0 adds:

1. **Per-module attribution** — which prompt produced each step (GEPA credit assignment)
2. **ATIF round-trip** — `Deserialize` path so reflection reads trajectories back in
3. **`FeedbackFn`** — scalar + natural-language feedback per module (GEPA's `µ_f`)

## Chatty mapping

| Paper concept | Chatty location | Status |
|---------------|-----------------|--------|
| Trajectory `(thought, action, observation)*` | `chatty-trace::Trajectory` | Reserved — human writes |
| `Action::Language` (ReAct's Â = A ∪ L) | `chatty-trace::Action` | Reserved |
| Per-step `ModuleId` | Extension over `AtifStep` | To build |
| ATIF export | `chatty-core` exporters (Serialize-only today) | Round-trip in M0 |
| Trace retention | `Recorder` trait, no-op in release | To build |
| Train vs validation rollouts | `RolloutBudget` over `token_budget/` | To build |

Capture attaches to the **existing** loop via `rig-agent` hooks + `rig-tap` — no
hand-rolled agent loop.

## Eval protocol

**Stage A:** HotpotQA run produces a `Trajectory` that round-trips; two seeded runs on a
warm cache are byte-identical.

**Stage B:** Harbor runs consume exported ATIF; attribution must survive export/import.

## Production landing

| Mechanism | Likely promotion |
|-----------|------------------|
| No-op `Recorder` in release | **Default** |
| Full step retention | **Setting** (`TrainingSettingsModel.atif_auto_export` already exists) |
| `FeedbackFn` implementations | Offline / optimizer only |

## Reserved symbols (human only)

`Trajectory`, `Step`, `Action`, `Outcome`, `FeedbackFn` — see [`RESERVED.md`](../../../RESERVED.md).

## Depends on

- [AGE-32](https://linear.app/agents-research/issue/AGE-32) rig 0.42 upgrade
- [AGE-22](https://linear.app/agents-research/issue/AGE-22) walking skeleton findings

## Further reading

- [crate-promises-chatty-trace](../crate-promises-chatty-trace.md)
- [Production bar (AGE-26)](https://linear.app/agents-research/issue/AGE-26)
