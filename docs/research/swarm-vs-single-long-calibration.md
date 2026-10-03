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

## Amendments (each committed before the runs it governs)

1. **Hold cap 150 s (2026-10-03 09:00, during round 1).** The first runs
   used `--hold-max-s 1800`. A request the meter holds for a server slot
   longer than 3 minutes trips Chatty's own stall watchdog ("the model
   stopped responding"), which ended one run (`lr01`, `cal-k1`) early: a
   harness artefact, not a model failure. Both streams were restarted with
   `--hold-max-s 150`; that run was discarded and re-run, as were the two
   runs in flight at the restart. The real run uses 150 s too.
2. **Round 2 of candidates (2026-10-03 10:00, after round 1's first 14
   single-arm runs).** Round 1 showed the single agent at 0 on every
   multi-module code task so far (it reads the 1.3k–2.5k-line package
   until the loop guard stops it, without editing) and at 0–0.33 on the
   long research corpora, while two data tasks scored 1.00. Too few tasks
   would survive, so new candidates are added, written without any
   swarm-arm data and calibrated by the same rule (k = 2, single arm only,
   run ids `cal2-k1`, `cal2-k2`):
   - `lc07`–`lc10`: small packages (500–600 lines, 5 issues each);
   - `lr07`–`lr12`: `lr01`–`lr06` with a short corpus (11k–14k words; same
     facts, questions and checks). Because each shares its questions with
     its long twin, at most one task of each twin pair enters the
     pre-registration: the long one if both are kept;
   - `ld07`–`ld09`: harder data tasks (longer data dictionary, rules
     amended over time, joins through effective-dated mappings).
   Round 1 runs to completion unchanged.

3. **Restart after a host reboot (2026-10-03 17:50).** The machine
   rebooted during round 2 and every calibration result (kept on `/tmp`)
   was lost. Calibration was re-run from scratch, rounds 1 and 2 together:
   all 31 candidates, k = 2, single arm only, with `--hold-max-s 150` from
   the start, results on the persistent disk
   (`/media/marcel/data/rust/swarm-results/ev7-cal/`, run ids `cal-k1`,
   `cal-k2`), on a build that also carries the newer `main` (chatty 0.5.10).
   The rules above are unchanged, including the twin rule. The record below
   is built only from these re-run results; the lost runs are not used.
   Still no swarm-arm run of any candidate exists.

## Record

Filled in after the calibration runs, before the pre-registration.
