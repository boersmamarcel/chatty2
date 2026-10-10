# AGE-862 Meta-Harness: resume notes

- **Prereg:** chatty2 branch `age-862-meta-harness`, `docs/research/meta-harness-prereg.md`, commit
  **2aba014b**, sha256 `2cd1c6b72f07c9711d022d3aceb9c8981facbfed28f80f5d72a85cb89ea22488`.
  Never edit it; amendments go in an addendum. `tasks.json` sha256 `1e32e50e…` (rule inside;
  `select_tasks.py` reproduces it).
- **Worktree** (persistent disk, no build needed): `wt/` = chatty2 branch `age-862-meta-harness`
  (from v0.7.0). Scripts: `wt/scripts/meta-harness/{mh.py,proposer.py,knobs.py}`.
- **Binary:** v0.7.0 = `d2d35264` (`TARBALL_SHA`); tarball in harbor-chatty-dabstep-team `dist/`;
  host copy `bin/chatty-tui-<sha>` (EV-7 path).
- **Harbor adapter:** `harbor-chatty-dabstep-team/agents/meta_harness.py` (`MetaHarnessArm`,
  untracked like the other AGE-853 adapters; a copy is in the branch as `harbor_meta_harness.py`).
- **EV-7 bench:** `ev7bench/` (copy of branch age-826-long-bench 91d9025e `scripts/swarm-bench` +
  `evals/swarm-long`); candidate applied via `heldout/<id>/chatty-tui-wrapper`.
- **Python:** `/media/marcel/data/rust/swarm-results/ev7-venv/bin/python` (system python3 is 3.6).

## What runs

- `start.sh` starts (idempotently) `meter.sh` (vLLM counters → `meter.log`), `mh.py queue`
  (log `queue.log`) and `proposer.py` (log `proposer/proposer.log`). `start.sh --no-proposer`
  for the queue alone.
- Queue priority (`mh.py next_job`): baseline search r1-r3 → `queue.txt` lines (`<cand> <set> <rep>`)
  → validated proposer candidates (search r1) → baseline test r1-r2, EV-7 r1-r2 → knob grid
  (on the baseline, then on the best proposer candidate). 2-wide on vLLM, one job at a time.
- Stop: `touch STOP` (queue finishes its job), `touch proposer/STOP`.
- Views: `cd store && ./mh status | frontier | top 5 | diff a b | show id`.

## Resume after a crash or reboot

`pgrep -af "mh.py queue|proposer.py|meter.sh|harbor (run|job)"`; if gone: `bash start.sh`.
A harbor job dir that exists is resumed with `harbor job resume`; finished jobs have `DONE`.
Killed harbor leaves containers: `docker ps` and stop only those of the dead job.
/tmp is not used by anything here.

## Pilot and end game (prereg §4)

- `proposer/checkpoint-5.json` holds the pilot verdict (band, candidates beating it);
  `proposer/STOP_PILOT` exists if the proposer was stopped by the rule.
- Finalists: top 5 by search-r1 → add `<id> search 2|3|4` lines to `queue.txt`; select on the
  mean of r2-r4; then `<id> test 1|2` and `<id> ev7 1|2`; analysis = paired bootstrap (prereg §5).

## Log
- 2026-10-10 ~10:00 tarball v0.7.0 built; tasks selected; prereg committed 2aba014b; smoke started.
