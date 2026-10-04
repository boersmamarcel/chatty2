# Pre-registration addendum: the "parallel" task family (EV-7, AGE-826)

**When to read this:** you are about to calibrate, run or read the result of
the six "parallel" tasks that were added to EV-7 after its main
pre-registration ([`swarm-vs-single-long-prereg.md`](./swarm-vs-single-long-prereg.md)).

This file is frozen before any parallel task is written, calibrated or run.
`long_parallel_addendum_is_frozen` (`crates/chatty-tui/tests/swarm_bench.rs`)
pins its SHA-256. Where it is silent, the main pre-registration applies.

## Why

The main task set rewards a long single thread of work. Teams are supposed to
win on broad work that splits into independent pieces. This family tests that
directly and is reported as a second family, never pooled into the main
headline.

## Tasks

Six candidates in `evals/swarm-long-parallel/tasks/` (kept apart from the
frozen main task set), each with 5–9 graded sub-parts and a deterministic
verifier (`scripts/swarm-bench/verify.py`, the `parts` check), each part
independent of the others:

- answer N independent questions across a large document set;
- audit 8 separate modules or files, each with one seeded flaw;
- similar broad, splittable work (for example, per-file data cleanups).

Swarm presets are the existing frozen ones of the matching family
(`research-brief`, `fix-and-verify`, `data-analysis`); no preset is changed.

## Calibration rule

Single arm only, k = 2 runs per candidate, same settings as the main run
(think off, temperature 0.3, 100 turns, seeds from `seed_for(task, i)` for
i = 1, 2). **Keep a task if the mean sub-part score of its two single-arm runs
is in [0.20, 0.70].** No other criterion, and no swarm-arm run of a candidate
before the kept set is fixed. Tasks may not be edited after their calibration.
Calibration runs are not reused in the analysis.

## Run and analysis

The kept tasks (expected 3–6) run k = 3 replicates per arm, replicates seeded
as in the main run, after the main run finishes, with the main run's
conditions. Analysis is `analyze.py`, unchanged, reported separately
(mixed model, paired task-mean difference with bootstrap CI, tokens per solved
sub-part). With so few tasks the family is descriptive: an interval that
includes 0 is "no detectable difference", and the 25-point smallest effect of
interest is stated, not tested.
