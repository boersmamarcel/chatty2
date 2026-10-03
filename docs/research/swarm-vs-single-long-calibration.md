# Calibration: the long multi-part task set (EV-7)

**When to read this:** you want to know how the tasks of the long
swarm-vs-single benchmark ([AGE-826](https://linear.app/agents-research/issue/AGE-826))
were chosen, and why the set cannot have been tuned to the team.

EV-4 ([`swarm-vs-single-2026-10-02.md`](./swarm-vs-single-2026-10-02.md))
hit the ceiling: both arms solved 30/30, so no difference could show. EV-7
uses long tasks with 6–9 independently checkable sub-parts each
(`evals/swarm-long/`) and keeps only the tasks where the single agent leaves
headroom. The candidates are run on the **single arm only**; no swarm-arm run
of any candidate exists before the pre-registration
([`swarm-vs-single-long-prereg.md`](./swarm-vs-single-long-prereg.md)) is
committed.

## Protocol (fixed before the first calibration run)

- **Candidates:** 18 tasks, six per family: `ld01`–`ld06` (multi-file data
  analysis with a report), `lc01`–`lc06` (a multi-module code fix), `lr01`–
  `lr06` (a multi-source brief over an offline corpus). Each reference
  solution scores 100 % and each untouched workspace at most 20 %
  (`evals/swarm-long/selfcheck.py`).
- **Runs:** arm `single` only, k = 2 per task (two run ids, `cal-k1` and
  `cal-k2`), on the local vLLM (`RedHatAI/Qwen3.8-27B-INT4`, think off,
  temperature 0.3), with the settings the real run uses: `--max-turns 100`,
  `--max-duration 45m`, hard kill at 50 min.
- **Score:** sub-parts passed ÷ sub-parts, by `scripts/swarm-bench/verify.py`.
- **Keep rule:** a task is kept when the mean of its two sub-part scores is
  in **[0.20, 0.70]**. A task above 0.70 is kept only when the single agent
  solved the whole task at most once of the two runs (headroom on full
  pass). A task below 0.20 is dropped (floor: no room to show a loss, and
  likely too hard for both arms).
- **No edits after calibration.** A dropped task is not reworded and
  re-tried; the kept tasks enter the pre-registration unchanged (their
  bytes are hashed there).
- **Shared server:** two calibration streams at most, behind the harness's
  shared throttle (at most 2 running + waiting requests on the server,
  serialised by a host-wide lock).

## Record

Filled in after the calibration runs, before the pre-registration.
