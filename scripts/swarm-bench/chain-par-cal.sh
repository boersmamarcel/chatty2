#!/bin/bash
# Waits until both EV-7 main streams have logged "finished", then calibrates the six "parallel" tasks
# (single arm, k=2, two streams, seeds as in the main run). Resumable: rerun the same command.
set -u
WT=${WT:-/tmp/swarm/age-826-long-bench}
M=/media/marcel/data/rust/swarm-results/ev7-run
O=/media/marcel/data/rust/swarm-results/ev7-par-cal
BIN=/tmp/age-826/bin/chatty-tui
[ -x $BIN ] || { mkdir -p /tmp/age-826/bin; cp /media/marcel/data/rust/swarm-results/ev7-bin/chatty-tui $BIN; }
until grep -q '^stream a finished' $M/ev7-run-a.log 2>/dev/null && grep -q '^stream b finished' $M/ev7-run-b.log 2>/dev/null; do sleep 300; done
mkdir -p $O; cd $WT
cal() { # stream tasks
  for R in 1 2; do
    python3 scripts/swarm-bench/bench.py --provider openai-compat --model RedHatAI/Qwen3.8-27B-INT4 \
      --think false --arm single --tasks evals/swarm-long-parallel/tasks --only $2 --rep $R \
      --max-turns 100 --max-duration 45m --run-timeout 3000 --hold-max-s 150 --throttle-max 2 \
      --prereg docs/research/swarm-vs-single-long-prereg-addendum-parallel.md --chatty-tui $BIN \
      --out $O --run-id par-cal-$1-r$R >> $O/par-cal-$1.log 2>&1
  done
  echo "stream $1 finished $(date -Is)" >> $O/par-cal-$1.log
}
cal a p01,p03,p05 &
cal b p02,p04,p06 &
wait
