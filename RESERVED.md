# RESERVED.md — functions the human writes

**This file is a contract, not documentation.** `scripts/check-reserved.sh` parses the
table below, so keep the row format intact. CI runs it.

## Why this exists

The research work landing in this workspace — the Linear project
**Self-improving chatty2** — has two outputs, and only one of them is code:

1. Five papers landed as working features of chatty2.
2. **A human who understands those five papers deeply enough to extend them.**

An agent that implements every issue perfectly delivers (1) and destroys (2). So a small
set of functions is reserved. **The rule of thumb: the human writes the ~200 lines that
contain the idea, the agent writes the ~2,000 lines around them.** Nobody learns anything
from a dataset loader.

This applies to the research crates only. Ordinary chatty2 work is unaffected.

## The hard rule

**Do not implement anything in the table below.** Not in a file, not in a scratch buffer,
not in a chat message, not as "here's roughly what it would look like". A worked solution
the human *reads* is the same as one he copies — the learning is in the struggle, and
showing the answer removes it either way.

When you reach a reserved function:

1. Leave `todo!("human: <one-line spec>")` in place.
2. **Write its failing tests.** This is the highest-value thing an agent does here — the
   human gets an executable spec and a tight feedback loop.
3. Build everything around it: types, wiring, error handling, the other functions.
4. Say clearly what you stubbed and what the tests expect.

Each reserved symbol is in exactly one of two states:

- **Not yet written** — the file contains `todo!("human: ...")`.
- **Written by the human** — the file carries `// HUMAN-WRITTEN: <symbol>` above it.

CI fails if a reserved symbol's file exists with neither marker — which is exactly what an
agent silently implementing it looks like. To lift a reservation, delete its row and say so
in the commit message.

## The list

| File | Symbol | Issue | Why it is reserved |
|---|---|---|---|
| `crates/chatty-trace/src/trace.rs` | `Trajectory` | AGE-22 / AGE-5 | The contract every optimizer is written against. Risk #1 in the plan. |
| `crates/chatty-trace/src/trace.rs` | `Step` | AGE-5 | One thought/action/observation triple. Its shape decides what reflection can attribute. |
| `crates/chatty-trace/src/trace.rs` | `Action` | AGE-22 / AGE-5 | `Â = A ∪ L` in the type system — the ReAct idea, encoded. |
| `crates/chatty-trace/src/trace.rs` | `Outcome` | AGE-5 | What "this rollout succeeded" means, and it is not a bool. |
| `crates/chatty-trace/src/feedback.rs` | `FeedbackFn` | AGE-22 / AGE-5 | GEPA's `µ_f`. The most-skipped detail in that paper: scalar-only feedback breaks every optimizer above it. |
| `crates/chatty-flow/src/repr.rs` | `WorkflowRepr` | AGE-13 | The seam that decides whether Monty code mode can ever replace the IR without rewriting the search. |
| `crates/chatty-flow/src/ir.rs` | `WfNode` | AGE-13 | What `IrRepr` can express decides whether the port succeeds. The week-6 kill criterion hangs on it. |
| `crates/chatty-optimize/src/archive.rs` | `SelectionStrategy` | AGE-7 | The shape AFlow, GEPA and DGM all instantiate. |
| `crates/chatty-optimize/src/aflow/select.rs` | `soft_mixed_select` | AGE-13 | λ=0.2, α=0.4, plus the blank-template guarantee that stops local optima. |
| `crates/chatty-optimize/src/gepa/select.rs` | `select_candidate` | AGE-15 | **GEPA's actual contribution.** ~30 lines, worth 6.4 points on the ablation. |
| `crates/chatty-optimize/src/gepa/merge.rs` | `merge` | AGE-15 | System-aware crossover: ancestry, desirability, per-module selection. |
| `crates/chatty-optimize/src/gepa/prompts.rs` | `REFLECTION_META_PROMPT` | AGE-15 | Appendix B's wording is why GEPA produces declarative instructions rather than quasi-exemplars. Copying it without reasoning about it teaches nothing. Do not quote the passage. |
| `crates/chatty-playbook/src/merge.rs` | `apply` | AGE-17 | **ACE's whole argument, as a type signature.** Pure, total, no LLM. ~40 lines. |
| `crates/chatty-playbook/src/refine.rs` | `grow_and_refine` | AGE-17 | Growth bounds and de-duplication. In a long-running desktop app this is an eviction policy, not a paper knob. |
| `crates/chatty-eval/src/strategy.rs` | `Strategy` | AGE-11 | The ReAct/CoT-SC backoff rule and the loop's termination semantics. The loop itself already ships — this is the one part of it that is a judgement call. |

## Explicitly NOT reserved

Build all of this without asking:

- Dataset loaders and scorers that feed *in-process* optimizers, the paired-statistics
  harness, and thin calibration stubs in `chatty-eval`
- Stage B sandboxes (HumanEval, Polyglot, AppWorld, ALFWorld) belong in the sibling
  `harbor-chatty` Harbor repo (AGE-34) — do not rebuild them inside chatty2
- The `IrRepr` interpreter and all seven AFlow operators
- Reflector and Curator wrappers, the Generator adapter, the `MonolithicRewrite` control
- Error taxonomies, `Recorder` impls, budget accounting, caching, telemetry, serde, CI
- Every ablation switch, and **the failing tests for every reserved symbol above**

## Two things agents get wrong here specifically

**1. Building what already exists.** `chatty-core` ships 60 `impl Tool` implementations
(`shell_tool`, `filesystem_write_tool`, `git_tool`, `execute_code_tool`, `search_tool`,
`search_web_tool`, `fetch_tool`, `sub_agent_tool`, `invoke_agent_tool`, `save_skill_tool`,
`search_memory_tool`), the ReAct loop (`llm_service.rs` + `agent_factory/`), a sandbox
(`src/sandbox/` — Docker via bollard, plus a python-subprocess backend under rlimits),
ATIF v1.6 export (`exporters/types.rs` — **`Serialize` only**; adding `Deserialize` is real
work and belongs to AGE-5), `src/token_budget/`, and a memory/skills store. If an issue
reads as "build a sandbox" or "write a `BashTool`", it is stale — say so instead of
building it.

**2. Following a retracted instruction.** An earlier draft of the plan claimed
`.multi_turn(n)` discards the trace and therefore the agent loop had to be hand-rolled.
**That was wrong and is retracted.** rig-agent 0.42's `AgentHook` / `HookContext` /
`AgentRun`, plus `rig-tap`, capture the loop without re-driving it. Any surviving "drive
the loop explicitly" or "not `.multi_turn()`" text is stale.

Also: rig 0.42 is a crate family and the lib names are `rig_core`, `rig_agent`, `rig_mcp`,
`rig_tap`. **There is no `rig` crate** — every `rig::...` path is wrong. The `Tool` trait is
`rig_agent::tool::Tool`; `Agent`, `AgentBuilder` and `Prompt` are also in `rig_agent`.

## DGM is not in this repo

The fifth paper, the Darwin Gödel Machine, self-modifies its target repository. That
repository must never be this one — chatty2 ships a desktop app with an auto-updater.
**A checked-out copy of this workspace does not satisfy the carve-out.** DGM lives in the
separate `agenticloop` repo, targets a standalone research repo, and borrows this repo's
sandbox and tools as dependencies. This applies to the cross-module experiments too
(AGE-21, experiment 4).

## Picking up work

Linear issues carry exactly one ownership label:

| Label | Meaning |
|---|---|
| `owner:ai` | Yours end to end. Boilerplate, harnesses, datasets, CI, docs. |
| `owner:pair` | Scaffold it and write the tests; leave the functions named in the issue's `## Ownership` section as `todo!()`. |
| `owner:human` | Do not implement. Support only. |
| `gate:reflection` | A learning gate. Never answer its questions, never close it. These carry no owner label — deliberately. |

**Only pick up `owner:ai` issues unless explicitly told otherwise.** If you were pointed at
the whole project rather than a filtered view, filter it yourself.

**Start at AGE-32** — the rig 0.37 → 0.42 migration. It blocks every other implementation
issue in the project.

## If the human overrides

He can lift any reservation — it is his project and his learning.

1. State the cost once, briefly, without nagging.
2. If he confirms, do it without further comment.
3. Note in the commit message or issue comment which reservation was lifted.

One push-back, then defer.

## Reference

- Working Agreement (full version) and the Master Research Plan: Linear project
  **Self-improving chatty2** → documents
- `RESERVED.md` in the `agenticloop` repo is the authoritative list for the DGM side
