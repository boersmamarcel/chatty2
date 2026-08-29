# M4 — ACE evolving playbooks

**Paper:** Zhang, Hu, Upasani, … Thakker, Zou, Olukotun — *Agentic Context Engineering*
([ICLR 2026](https://doi.org/10.48550/arXiv.2510.04618))

**Linear:** [AGE-9](https://linear.app/agents-research/issue/AGE-9) · **Crate:**
`chatty-playbook` · **Promotion:** pending

## Mechanism

Three roles over one **playbook** artifact:

- **Generator** — runs task, reports which bullets were used (`bullet_ids`)
- **Reflector** — tags bullets helpful/harmful/neutral from execution feedback (≤5 rounds)
- **Curator** — emits JSON delta ops (`ADD`, `UPDATE`); never rewrites whole playbook

**Merge is deterministic code** — no LLM in `apply()`. Bullet format:
`[ctx-00263] helpful=1 harmful=0 :: <content>` in named sections.

**Grow-and-refine** bounds size via de-duplication; paper uses loose thresholds (10K–100K
token prune trigger) — in Chatty this becomes a **production eviction policy**, not a knob.

Two failure modes to reproduce as negative controls:

- **Brevity bias** — optimizers drop domain heuristics (ACE cites GEPA's shorter prompts)
- **Context collapse** — monolithic rewrite shrinks artifact (AppWorld: 18K tokens → 122)

## Chatty mapping

| Paper concept | Chatty location | Status |
|---------------|-----------------|--------|
| Facts / procedures store | `memory_service`, `remember_tool`, `save_skill_tool` | **Ships** |
| Sectioned playbook | `chatty-playbook::Playbook` | To build |
| Delta merge | `chatty-playbook::apply` | Reserved |
| Grow-and-refine | `chatty-playbook::grow_and_refine` | Reserved |
| Generator | Existing running agent | Ships |
| Reflector / Curator | New `rig_agent::Agent`s | To build |
| Stable section order | `BTreeMap` serialization | Required for prompt-cache cost story |

**Product prize:** playbook grows from execution feedback, not only when the agent calls
`remember`.

## Eval protocol

**Stage A:**

- `apply` pure: ADD, UPDATE counters, no-op on unknown id, parallel deltas commute
- Byte-stable serialization across no-op update (prompt-cache invariant)
- Context-collapse control reproduces collapse curve
- Label-free modes including degradation case (FiNER 70.7 → 67.3 without labels)

**Stage B:** AppWorld (primary), Finance (FiNER, Formula) — Marcel posts numbers.

ACE vs GEPA rollout comparison → [AGE-21](https://linear.app/agents-research/issue/AGE-21),
not this module alone.

## Production landing

| Mechanism | Likely promotion |
|-----------|------------------|
| Deterministic `apply` + runtime playbook | **Setting** (scope TBD) |
| Playbook scope | Global / per-model / per-conversation ([AGE-47](https://linear.app/agents-research/issue/AGE-47)) |
| Eviction policy | **Default** bounds required for desktop app (not paper's 10K–100K knob) |
| Offline multi-epoch warmup | Optimizer feature, not request path |

## Reserved symbols

`apply`, `grow_and_refine` — see [`RESERVED.md`](../../../RESERVED.md).

## Depends on

- [M0 Trace](./m0-trace.md), [M1 ReAct](./m1-react.md)
- Independent of M2 — can parallel with AFlow

## Further reading

- [crate-promises-chatty-playbook](../crate-promises-chatty-playbook.md)
- [agent-memory.md](../../agent-memory.md)
