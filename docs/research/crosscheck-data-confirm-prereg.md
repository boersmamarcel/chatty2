# Pre-registration: Crosscheck: Data with the DABstep helpers (AGE-853, run 2)

**When to read this:** You are about to run, or read the result of, the
confirmatory DABstep run of the `crosscheck-data` team with the DABstep
helper code and brief, or you want to quote a DABstep number for it.

This file is frozen once committed; its commit hash is recorded in
`/media/marcel/data/rust/swarm-results/age-853-c2/RESUME.md` and in the
result note. A run made after any change to it is not pre-registered. The
protocol follows [`experiment-protocol.md`](./experiment-protocol.md).

Written 2026-10-05, after run 1
([`crosscheck-prereg.md`](./crosscheck-prereg.md)) and before any run of
this one.

## What run 1 showed, and why run 2

Run 1 (no helpers, 18 mixed + 30 no-harm tasks): arm A single 4/18 and
5/30, arm B Crosscheck: Data 7/18 and 4/30, 2.1× the tokens on vLLM's
counters. It failed the no-harm gate. It also ran into a defect that this
run fixes first: inside a task container the three solvers share
`/app/answer.txt`, and a worker stopped its run as soon as *any* answer
file existed, so once one solver wrote it the others (and the judge) were
cut off. 31 of 39 recorded `best_of` results fell back to the vote because
the judge returned nothing, and several attempts' answers were a first word
("The", "I", "Let"). Run 1 therefore measured a broken arm B. The fix
(branch `age-853-worker-answer-file`): a worker stops on the answer file
only after a tool call of its own that names it.

Run 2 also gives both arms the DABstep helper code and brief, the setting
in which our 84.9 % Hard result was measured, so its arm-B score on held-out
Hard tasks can be read against that result.

## Question

With the DABstep helpers, does Crosscheck: Data solve more of the tasks on
which single runs disagree than one run does, without losing accuracy
elsewhere, at no more than 4.5× the tokens?

## Task set (fixed)

`/media/marcel/data/rust/swarm-results/age-853-c2/tasks.json`, pinned by
content hash:

- **Mixed (18):** the same 18 as run 1 (adyen/10, 18, 32, 38, 43, 49, 58,
  63, 66, 67, 69, 72, 178, 341, 347, 375, 471, 617).
- **No-harm (100):** `random.Random(8530).sample(pool, 100)`, `pool` being
  the 450 tasks minus subset-80, minus the 40 held-out tasks of
  `phase7-conv-heldout40`, minus run 1's 30 no-harm tasks, sorted by task
  number (303 tasks). All 100 are Hard (every Easy task is in subset-80).

Contamination, disclosed: the helper's conventions were mined partly from
judge rationales on subset-80, which contains the 18 mixed tasks. The
no-harm 100 are outside subset-80 and outside the held-out-40 used to gate
the helper versions, so they are held out from both.

## Arms (one draw per task and arm)

Same for both: `RedHatAI/Qwen3.8-27B-INT4` on the local vLLM (32k context),
`--think false`, the `chatty-tui` tarball built from branch
`age-853-worker-answer-file` (sha recorded in `RESUME.md` before the run),
the Harbor DABstep task containers, and in each workspace the files from
the AGE-754 harness (harbor-chatty commit 38b4bc8, `dabstep/`): `BRIEF.md`
(sha256 42331fb1…) as `/app/BRIEF.md` and helper v4 `helper.py` (sha256
213f3949…) as `/app/helper.py`, with `duckdb` and `pandas` installed.
Agent timeout 2400 s.

- **A, single:** one agent with AGE-754's single-agent preamble
  (`single-preamble.md`, sha256 6541e4ae…: read BRIEF.md first, compute with
  code, answer with `final_answer`). Two trials at a time.
- **B, Crosscheck: Data:** `--team crosscheck-data`: three analysts at once
  (endpoint budget 3), judge with thinking off, writer. One trial at a time.
- **C, vote (offline):** over B's three attempts as `best_of` reports them,
  the most common normalised final answer, a tie going to attempt 1, scored
  with each task's own `tests/scorer.py`. No new model call.

## Metrics

- **Success:** the task verifier's reward. A Harbor or container failure is
  re-run once; a second failure, a timeout or no answer file is 0.
- **Tokens:** vLLM's own counters (prompt + generation tokens), logged every
  15 s to `meter.log`, differenced over each arm's window; nothing else uses
  vLLM during the run. (Worker usage does not reach the leader's usage file
  in the containers, AGE-854.) Arm A's usage files are the cross-check.
- **Wall time**, reported, not gated.

## Gates (fixed)

Crosscheck: Data's claim ("it helps on data questions") is supported only if
all three hold:

1. **Mixed:** B − A ≥ +10 points on the 18 mixed tasks (B solves at least 2
   more).
2. **No harm:** on the 100 no-harm tasks, the lower bound of the 95 %
   paired-bootstrap confidence interval of B − A (10,000 resamples of the
   paired per-task differences, seed 853, percentile method) is ≥ −5 points.
3. **Cost:** B's tokens ≤ 4.5 × A's, over all 118 tasks.

C does not gate; B against C on the mixed set shows whether the judge earns
its place.

## Claim 2 (estimate, not a gate)

B's and A's scores on the 100 no-harm tasks, with a 95 % bootstrap CI, are
reported as estimates of held-out Hard accuracy with the helpers, next to
our 84.9 % Hard (full 450, k=2 judge, 2026-09-18) and NVIDIA's published
89.95 % Hard (KGMON + Claude Haiku 4.5, Hugging Face blog, 2026-03-13). All
our numbers are local grading: each task's verifier runs the adapted
DABstep scorer against the Harbor adapter's answer key, not the
leaderboard; nothing is submitted. A 100-task sample is not the full set,
and the note says so next to the number.

## Reporting

`docs/research/crosscheck-data-confirm-<date>.md`: the gate verdicts with
the bootstrap CI, paired wins and losses per set, the token table from
`meter.log`, wall time, B against C, the claim-2 table, run 1's result and
its defect, and what the run cannot show.
