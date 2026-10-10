# AGE-862 Meta-Harness for chatty: pre-registration

Frozen before any candidate is evaluated. Never edit this file after the commit that adds it;
amendments go in a separate addendum with its own hash.

- Issue: AGE-862. Paper: Lee et al. (2026), *Meta-Harness: End-to-End Optimization of Model
  Harnesses*, arXiv:2603.28052 (vault `raw/papers/2026-10-10-meta-harness-end-to-end-optimization-of-model-harnesses.md`).
- Binary: chatty-tui at chatty2 `v0.7.0` = `d2d35264b2b1184a275d2fb7736690bc97dd79f3`, amd64
  tarball built by `harbor-chatty-dabstep-team/build/build-amd64.sh`, run with `--export-atif`.
- Model: `RedHatAI/Qwen3.8-27B-INT4` on the local vLLM (http://172.17.0.1:8000/v1), never
  restarted, at most 2 concurrent requests (Harbor `n_concurrent_trials: 2`; EV-7 bench throttle 2).
- Results and scripts: `/media/marcel/data/rust/swarm-results/age-862/` (`RESUME.md`);
  harness code `scripts/meta-harness/` on chatty2 branch `age-862-meta-harness`; Harbor adapter
  `agents/meta_harness.py` in `harbor-chatty-dabstep-team`.

## 1. Literature check (what we carry over and what differs)

- The paper's proposer (Claude Code, Opus) reads a filesystem of every prior candidate's code,
  scores and raw execution traces; the ablation (text classification, Table 3) has the full
  interface at median 50.0 / best 56.7 against scores-only 34.6 / 41.3 and scores+summary
  34.9 / 38.7. We keep the full-trace interface (ATIF per task) and a minimal skill that says
  where to write, how to inspect, and what may not be touched; candidates pass interface
  validation before evaluation; selection uses the search set only; test sets are held out.
- A typical paper run evaluates ~60 harnesses over 20 iterations. Our budget: 20 iterations x
  k=2 proposer candidates, plus knob-search candidates that fill idle GPU time.
- Differences that lower our odds, stated up front: (a) our candidates are declarative (preamble,
  workspace conventions, helper code, skills, CLI knobs), not arbitrary harness code; context
  compaction thresholds and tool descriptions are compiled into v0.7.0 and are outside phase 1;
  (b) our eval model is a local 27B INT4, theirs frontier/API models; (c) our tasks are agentic
  and stochastic, so a single 40-task pass is noisy (hence the noise band and finalist re-runs);
  (d) TerminalBench-2 in the paper used the same tasks for search and test; we do not.
- Prior: the hand-run register E0-E7 (stopped 2026-09-28) found no gain in 48 h; `ruled-out.md`
  in the store gives the proposer that register.

## 2. Data (tasks.json, sha256 `1e32e50eb0621ff1490dbc9403301168b994dc459ce19fe2db963619ef0aaad8`)

- **Search set (40 DABstep Hard):** `random.Random(8620).sample` of the 72 Hard tasks that
  age-853-c2 arm A (the baseline setup, one run) got wrong, or whose age-853 run-1 arm-A reward
  disagreed with c2 arm A. Selected on failure, so the baseline's re-run score on it is expected
  to regress upward; the reference is therefore the baseline's own 3 fresh re-runs, never the
  selection run.
- **Test set (100 DABstep Hard):** `random.Random(8621).sample` of the 203 Hard tasks outside
  age-853-c2's 118 (mixed 18 + no-harm 100), age-853 run 1's 48, subset-80 (helper.py was mined
  on it), phase-7 held-out-40 and the search set. Overlap with the search set and with c2's
  no-harm 100: none.
- **Second domain (EV-7):** the 15 calibrated swarm-long tasks (streams a: ld01 lr01 lc08 ld04
  lr06 lc10 ld07 ld09; b: lc07 ld03 lr03 lc09 ld06 lr08 ld08), single arm, the EV-7 bench
  (`bench.py`) with its metering proxy: temperature 0.3, per-(task, rep) seed `seed_for`, same in
  every candidate; `--max-turns 100 --max-duration 30m`. Score = the bench's sub-part fraction.
  DABstep runs have no per-request seed control in the Harbor adapter (vLLM default sampling);
  their noise is handled by repetitions, not seeds.
- The proposer's working directory (`store/`) holds search-set results and traces only; test-set
  and EV-7 results live in `heldout/`, outside it, and the skill forbids reading them.

## 3. Arms and search space

- **Baseline `c000-baseline`:** the age-853-c2 arm-A setup (single run, `BRIEF.md`, `helper.py` v4,
  the single preamble as `--preamble`, `duckdb pandas`, `--think false`) on v0.7.0 built-ins,
  no other flags.
- **Candidate** = a directory: `preamble.md`, `BRIEF.md`, `helper.py`, `skills/<name>/SKILL.md`,
  `knobs.json` (`max_agent_turns`, `max_duration`, `tool_loading`, `tools`, `only`, `think`).
  Same binary, same model, same tasks. On EV-7 only the domain-general parts apply (preamble,
  skills, knobs); `BRIEF.md`/`helper.py` are DABstep workspace files.
- **Proposer:** Claude Code headless, model Opus, the `meta-harness-proposer` skill, k=2 candidates
  per iteration. Forbidden: task ids, dataset constants, hard-coded answers, grader/verifier
  access, the test set and EV-7 results. `mh.py validate` rejects a candidate that adds a task id,
  grader terms or a file outside the allowed set, or lacks a one-line rationale per change.
- **Knob search (hybrid):** a fixed one-factor grid (`KNOB_GRID` in `mh.py`: max_agent_turns
  12/20/40, max_duration 15m/20m, tool_loading dynamic, think true, only
  shell,fs-read,fs-write,code-exec) on the baseline, then on the best proposer candidate. It runs
  only when no proposer candidate is waiting, so it fills GPU time.

## 4. Procedure

1. Smoke: baseline on 2 search tasks (adapter, ATIF export, scoring). Not used in any analysis.
2. Baseline: search set x3 (noise band = min..max of the 3 rep means), test set x2, EV-7 x2.
3. Pilot: 5 proposer iterations (10 candidates), each scored once on the search set.
   **Pilot rule (from the issue):** if no candidate beats the baseline on the search set beyond its
   noise band (search-r1 mean > the band's max), stop the proposer and report. Otherwise continue
   toward 20 iterations. **Abort:** no candidate beats the baseline on the search set by iteration 10.
4. Finalists: the top 5 candidates by search-r1 mean (proposer and knob, ties broken by fewer
   tokens per task) are re-run 3x on the search set (r2-r4). Selection uses the mean of r2-r4
   only (fresh runs, against the winner's curse); the selected candidate must also be on the
   accuracy/tokens Pareto frontier of those fresh means. If no finalist's fresh mean exceeds the
   baseline's 3-rep mean, nothing goes to the test set and the result is negative.
5. The selected candidate runs the test set x2 and EV-7 x2; the gate below is applied.

## 5. Analysis

- Per task, the mean reward over that arm's reps (test: 2 per arm; EV-7: 2 per arm). Difference
  = candidate - baseline in percentage points. 95 % paired bootstrap over tasks, 10 000 resamples,
  `random.Random(862)`, percentile interval.
- Tokens = `usage.json` input + output tokens per trial (vLLM `meter.log` as cross-check);
  tokens per solved task = total tokens / sum of rewards, on the test set.
- Power (assumption, re-estimated from the baseline reps when they land): within-task run variance
  ~0.15 for binary rewards, k=2 per arm, n=100 gives SE(diff) ~3.9 points and a 95 % CI
  half-width ~7.6 points. Gate 1 then has ~25 % power for a true +5 point effect, ~50 % for
  +8 and ~70 % for +10. On the 40-task search set the SD of one rep mean is ~6 points, so with
  10 pilot candidates a "beats the band" result is likely by chance alone (~80 %); the pilot rule
  only decides whether to continue, the finalist re-runs and the held-out test decide the claim.

## 6. Production gate (verbatim from AGE-862; all must hold)

1. **Accuracy:** on the held-out DABstep test set (100), the best frontier candidate beats the baseline by **≥ +5 points**, and the 95 % paired-bootstrap CI lower bound is above 0.
2. **No harm elsewhere:** on the EV-7 second-domain tasks, the 95 % CI lower bound of the difference is **≥ −5 points**.
3. **Cost:** tokens per solved task are **≤ 1.2×** the baseline's.
4. **Inspectable and general:** a review of the winning diff finds no task ids, dataset-specific constants, hard-coded answers or grader leakage. Every change has a one-line rationale tied to observed traces.
5. **Engineering:** the change lands as a normal built-in spec or config change, with no feature flag and no shims. The full test suite passes.
6. **Marcel signs off** on the review in (4) before the release.

If the gate fails, nothing ships. Write the result up in `docs/research/` and in a vault debrief, including which harness surfaces the proposer changed and why.

## 7. Run hygiene

- A trial killed by a host or server failure is re-run once; runs are never re-run for their
  outcome. Pre-registered runs get no partial reads beyond the pilot rule's checkpoint.
- Nothing ships from this run without gate 6.
