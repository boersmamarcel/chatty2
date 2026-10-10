# AGE-862 Meta-Harness: prereg amendment 1 (larger test set)

Amends `meta-harness-prereg.md` (commit 2aba014b, sha256 `2cd1c6b72f07c9711d022d3aceb9c8981facbfed28f80f5d72a85cb89ea22488`).
Decided by Marcel on 2026-10-10, before any test-set run had started: the queue was still on
baseline search r1, and no test-set or EV-7 result existed. The gates (§6) are unchanged.

## Reason

With 100 test tasks at k=2 per arm, gate 1 had about 25 % power for a true +5 point effect
(prereg §5). The test set is enlarged to every remaining unused DABstep Hard task.

## New test set (tasks.json sha256 `9900c1e178960373ea7a5d4464abd96408d32cf6e86a5e1cd52b6737384022d4`)

- `test` = **all 203 tasks** of the pool. There is no sampling, so no seed is needed; tasks are in
  task-number order. The pool is `sorted(Hard 378 minus age-853-c2's 118 (mixed 18 + no-harm 100)
  minus age-853 run 1's 48 minus subset-80 minus phase-7 held-out-40 minus the search set)`.
  The original seed-8621 100 are a subset, kept in `tasks.json` as `test100_original`.
- The count is 203, not the ~230 first estimated. The exclusions of the original prereg still
  hold: helper.py v4 was mined on subset-80 and chosen on held-out-40, and c2/run-1 tasks
  carry earlier results of this baseline setup. Overlap with the search set and with c2's
  no-harm 100 is still none.
- Runs: the baseline and the selected candidate each run the 203 tasks k=2 (about 406 trials per
  arm, roughly 9-10 h per arm at the 2-wide pace of 2.75 min per task).

## New power estimate (same assumptions as prereg §5)

Within-task run variance ~0.15, k=2 per arm, n=203: SE(diff) ~2.7 points, 95 % CI half-width
~5.3 points. Gate 1 (diff >= +5 and CI lower bound > 0): power ~45 % for a true +5, ~84 % for
+8 and ~96 % for +10. It is re-estimated from the baseline's two test reps when they land.
