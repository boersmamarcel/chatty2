# Pre-registration: Crosscheck on DABstep (AGE-853)

**When to read this:** You are about to run, or read the result of, the
Crosscheck verification on DABstep, or you want to quote a DABstep number
for Chatty next to a published frontier result.

This file is frozen once committed: its commit hash is recorded in
`/media/marcel/data/rust/swarm-results/age-853/RESUME.md` and in the result
note. A run made after any change to it is not pre-registered. The protocol
follows [`experiment-protocol.md`](./experiment-protocol.md).

Written for [AGE-853](https://linear.app/agents-research/issue/AGE-853) on
2026-10-04, before any real-model run of Crosscheck.

## Question

Do several independent attempts at one data question, with a judge that
reads their traces and picks one, answer DABstep more often than one run,
at a cost the user can see and accept?

- **H0:** Crosscheck: Data solves the same share of tasks as one run.
- **H1 (one-sided):** it solves more on tasks where single runs disagree,
  and no fewer elsewhere.

## Why we expect a gain here, and where not

- On sequential work, coordination teams have not bought accuracy: EV-4
  tied at 30/30 with 3.3× the tokens, and Kim et al.
  ([arXiv 2512.08296](https://arxiv.org/abs/2512.08296)) report multi-agent
  setups degrading sequential tasks.
- Independent samples plus a selector do pay: coverage rises with k
  (Large Language Monkeys), and sampling plus voting gains (*More Agents Is
  All You Need*).
- Our own evidence (AGE-9 follow-up, 2026-09-17): on the 18 DABstep tasks
  where three phase-5 attempts disagreed, a same-model trace judge with
  thinking off picked a right answer on 16/18, against 9/18 for majority
  vote. A thinking judge did worse (14/18).
- Counter-evidence: the `analyst-panel` preset (AGE-754,
  [`dabstep-team-2026-09-30.md`](./dabstep-team-2026-09-30.md)) did not
  beat one agent on subset-80 (63/80 against 64/80, 3× the tokens). Its
  largest loss bucket was invalid handoffs between a leader model and its
  workers (32 tasks). Crosscheck moves the fan-out and the selection out of
  the leader model into the `best_of` tool, and drops the analysts' handoff
  schema, so that bucket should be gone. Whether the rest of the gain
  survives is what this run measures.

## What is measured

### Task set (fixed)

48 DABstep tasks from the Harbor registry (`adyen/<n>`), pinned by content
hash in `/media/marcel/data/rust/swarm-results/age-853/tasks.json`:

- **Mixed set (18):** the tasks of subset-80 on which phase 5's three
  attempts (job `phase5-testtime-k3-subset80`) did not all score the same:
  adyen/10, 18, 32, 38, 43, 49, 58, 63, 66, 67, 69, 72, 178, 341, 347, 375,
  471, 617. Recomputed from the job's `result.json` rewards on 2026-10-04;
  it is the same 18 as AGE-9's.
- **No-harm set (30):** `random.Random(853).sample(pool, 30)` where `pool`
  is the 450 tasks minus subset-80, sorted by task number:
  adyen/1343, 1385, 1431, 1438, 1444, 1460, 1470, 1475, 1514, 1641, 1699,
  1700, 1705, 1734, 1740, 1747, 1757, 1765, 1780, 1810, 2264, 2397, 2549,
  2561, 2564, 2587, 2719, 2725, 2740, 2764.

### Arms (one draw per task and arm)

Same model for every arm: `RedHatAI/Qwen3.8-27B-INT4` on the local vLLM
(32k context), `--think false`, the same `chatty-tui` binary built from the
AGE-853 branch, the same Harbor task containers, no DABstep helper code, no
conventions file, nothing in the workspace but what the task ships.

- **A, single:** `chatty-tui --headless` as one agent, the default agent
  (no `--team`, no `--agent`). Two trials at a time.
- **B, Crosscheck: Data:** `chatty-tui --headless --team crosscheck-data`.
  The leader calls `best_of` once; three analysts (direct, plan-first,
  verify-first) run at once (endpoint budget 3, one trial at a time), the
  judge (`crosscheck-judge`, thinking off by its spec) picks when their final
  answers differ, the writer writes `/app/answer.txt`.
- **C, majority vote (offline, optional):** over B's three attempts, the
  most common final answer after the same normalisation `best_of` uses;
  a tie goes to attempt 1 (direct). Read from B's saved conversation (the
  `best_of` tool result), scored with the task's own `tests/scorer.py`
  against its expected answer. No new model call.

Adapter: `agents/chatty_crosscheck.py` in
`/media/marcel/data/rust/chattyapp/harbor-chatty-dabstep-team`, which adds
`--usage-file` and `--save-conversation` to every run and copies both files
into the trial directory. Agent timeout 2400 s per trial in both arms.

### Metrics

- **Success:** the task verifier's reward (1 or 0). An infrastructure
  failure (container, Harbor) is re-run once; a second failure, a timeout
  or a missing answer file counts as 0 for that arm.
- **Tokens:** `input_tokens + output_tokens` from the run's usage file,
  delegations included.
- **Wall time** per trial, reported, not gated (B runs one trial at a time,
  A two).

## Ship gate (from the issue, fixed)

Crosscheck: Data ships as a documented preset only if all three hold:

1. **Mixed:** B ≥ A + 10 points on the mixed set (18 tasks: at least 2
   more solved tasks).
2. **No harm:** B ≥ A − 2 points on the no-harm set (30 tasks: B solves at
   least as many as A, since one task is 3.3 points).
3. **Cost:** B's total tokens over the 48 tasks ≤ 4.5 × A's.

If any fails, the preset stays experimental and the result is written up
anyway. Arm C decides nothing; it shows whether the judge earns its place
(B against C on the mixed set). The generic `crosscheck` team is not
measured here and stays experimental whatever this run shows.

With 18 paired tasks this is a coarse gate: a +10-point difference is two
tasks. It is the issue's gate, kept as written; the note reports the paired
wins and losses so a reader can see how close it was.

## Claim 2: same accuracy at lower cost (comparison plan)

Marcel's second claim compares a published frontier result with ours, like
for like. We run no paid model for it.

### The published baseline

- **Source:** NVIDIA, "NeMo Agent Toolkit Data Explorer: 1st place on
  DABstep", Hugging Face blog, published 2026-03-13
  (<https://huggingface.co/blog/nvidia/nemo-agent-toolkit-data-explorer-dabstep-1st-place>).
  Their KGMON agent with Claude Haiku 4.5 at inference: **Hard 89.95 %,
  Easy 87.5 %**, about 20 s per task. Claude Opus 4.5/4.6 ran a learning
  phase "against ground truth" that distilled a `helper.py` of reusable
  functions and a few-shot set, and an offline reflection phase; inference
  sees the helper's signatures. They publish no token or dollar figure.
- **Split and scoring:** the DABstep leaderboard's Hard and Easy columns over
  the full 450 tasks, scored by the leaderboard against its private answers.
  The Validated track was closed to new entries when last checked
  (2026-09-17 clipping, not re-checked live).

### Our number and how it was scored

- **84.9 % Hard (321/378), 85.1 % overall (383/450):** the full 450, two
  independent single-agent draws (jobs `phase3-capstone` and
  `phase6-attempt2` in `harbor-chatty-dabstep-explore`, same config), and a
  same-model judge with thinking off over the 196 tasks where the two
  answers differed (2026-09-18). Single draws scored 73.7 % and 77.2 % Hard.
  It is **not** a Crosscheck preset run: it predates the preset and used
  an offline judge script, so it is quoted as a result of the harness with
  a k=2 judge, not of the team.
- **Scoring is local, not official.** Each Harbor task's `tests/test.sh`
  embeds an expected answer from the Harbor DABstep adapter and runs
  `tests/scorer.py`, which says it is adapted from the official DABstep
  scorer. Whether the adapter's answer key equals the leaderboard's hidden
  key is unverified. So the number is "84.9 % on our local grading" until
  Marcel decides on a leaderboard submission; we submit nothing.
- **Task-specific tooling, disclosed:** those draws used a DABstep
  `helper.py` and its signatures (v1), derived from the dev split and the
  manual, never from task answers, through `ChattyHelperAgent`. That is the
  same kind of tooling NVIDIA's entry uses (a distilled helper plus
  signatures), so the comparison is like for like on tooling; NVIDIA's
  helper was learned by a frontier model, ours was written in the AGE-9
  sessions. The helper files lived in `/tmp/dabstep-phase1` and were lost
  in the 2026-10-03 reboot; the job configs record their paths.
- **What was lost:** the judge's per-task picks (`/tmp/age9-judge`) and
  every token count (the adapter recorded none). The 84.9 % figure stands
  on the memory and vault records of 2026-09-18 and can be reproduced from
  the two job directories with `jobs-config/judge/phase6_pipeline.py`.

### Cost per solved task

- **Ours:** the GPU time a task holds, priced by a stated local cost.
  Assumption: one RTX 3090 at €1,500 written off over 3 years at 50 %
  utilisation (€0.11 per GPU-hour) plus 0.40 kW at €0.30/kWh (€0.12 per
  GPU-hour): **€0.23 per GPU-hour**. A task's GPU-hours are its wall time
  divided by the trials sharing the GPU. For the 84.9 % result, wall time
  comes from the two jobs' `agent_execution` timestamps plus the judge's
  recorded ~40 s per judged task. Tokens per task are taken from this run's
  arm A and B usage files, since the old jobs recorded none.
- **Frontier:** Anthropic's list price for Claude Haiku 4.5 on the result
  date (quoted with its URL in the result note) × tokens per task. NVIDIA
  publishes no tokens, so the assumption is our own arm A mean tokens per
  task, shown with a 0.5×–2× sensitivity range. Their learning phase
  (Opus) is a one-off cost and is left out, which favours them.
- **When we may claim it:** only if our Hard score is within the
  uncertainty of theirs (we treat a gap of up to 5 points as within it,
  given one draw on 378 tasks) **and** our cost per solved task is lower
  over the whole sensitivity range. Otherwise the note reports what was
  found and makes no cost claim.

### A run on the published split

The 84.9 % is already on the published split (all 450, Hard reported
apart), so no new run is needed for the accuracy half. A full-450 run of
the Crosscheck: Data preset itself (about 45 h of GPU time) is not part of
this pre-registration; it is Marcel's call after this result.

## Reporting

`docs/research/crosscheck-<date>.md`: the gate verdict per criterion,
paired wins and losses per set, a token-cost table per arm (total, mean
per task, multiple of A), wall time, B against C on the mixed set, the
claim-2 table with every assumption, and what the run cannot show.
