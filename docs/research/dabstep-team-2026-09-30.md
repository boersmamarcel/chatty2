# DABstep: analyst-panel against a single agent (E8)

**When to read this:** You want to know whether the `analyst-panel` team
answers DABstep better than one agent, or you are about to claim that it does.

Measured for AGE-754 on 2026-09-29/30 on DABstep subset-80 (72 Easy, 8 Hard),
80 paired tasks, one trial per task and arm. Model: Qwen3.8-27B-INT4 on local
vLLM, 32k context, `think=false`. Team: `crates/chatty-core/teams/analyst-panel`
(three analysts, an adjudicator and a lead).

## Result

| Arm | Hard | Easy | Overall | Errored | p50 s/task | p95 s/task | tokens/task (mean) |
|---|---|---|---|---|---|---|---|
| analyst-panel | 3/8 = 37.5% | 60/72 = 83.3% | 63/80 = 78.8% | 0 | 216 | 686 | 418k |
| single | 5/8 = 62.5% | 59/72 = 81.9% | 64/80 = 80.0% | 0 | 23 | 160 | 136k |

- On Hard, the single agent alone solved `adyen/178` and `adyen/375`. The panel won no Hard task that the single agent missed.
- On Easy the arms split: the panel alone solved 5 tasks, the single agent alone solved 4.
- The panel costs about 3× the tokens and takes about 9× as long at p50.

**Verdict: no gain.** The panel does not beat a single agent here. Do not claim
that a team beats a single agent in docs or marketing. The team stays
experimental.

## Where the panel loses

| Failure category (panel) | Tasks |
|---|---|
| invalid handoff: analysts | 22 |
| invalid handoff: adjudicator | 10 |
| adjudicated, not unanimous | 20 |
| wrong answer | 17 |

Invalid handoffs are the largest bucket: an analyst or the adjudicator returned
output that the next step could not use. That is a team-plumbing problem, not a
reasoning one. Fix the handoff format before re-running this comparison.

## Reproduce

The harness lives outside this repo, in the AGE-754 Harbor job directory:

```bash
python3 scripts/panel_report.py jobs/panel-subset80/panel-subset80 jobs/single-subset80/single-subset80
```
