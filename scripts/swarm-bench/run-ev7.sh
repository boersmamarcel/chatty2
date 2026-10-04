#!/bin/bash
# EV-7 main run, one stream: run-ev7.sh a|b   (resumable: finished runs are skipped)
# Replicates 1..3 in order; run ids ev7-run-<stream>-r<i>. Same command after a crash/reboot resumes.
set -u
S=$1
WT=${WT:-/tmp/swarm/age-826-long-bench}
O=/media/marcel/data/rust/swarm-results/ev7-run
BIN=/tmp/age-826/bin/chatty-tui
[ -x $BIN ] || { mkdir -p /tmp/age-826/bin; cp /media/marcel/data/rust/swarm-results/ev7-bin/chatty-tui $BIN; }
A=ld01,lr01,lc08,ld04,lr06,lc10,ld07,ld09
B=lc07,ld03,lr03,lc09,ld06,lr08,ld08
L=$([ "$S" = a ] && echo $A || echo $B)
mkdir -p $O; cd $WT
for R in 1 2 3; do
  python3 scripts/swarm-bench/bench.py --provider openai-compat --model RedHatAI/Qwen3.8-27B-INT4 \
    --think false --arm both --tasks evals/swarm-long/tasks --only $L --rep $R \
    --max-turns 100 --max-duration 45m --run-timeout 3000 --hold-max-s 150 --throttle-max 2 \
    --prereg docs/research/swarm-vs-single-long-prereg.md --chatty-tui $BIN \
    --out $O --run-id ev7-run-$S-r$R >> $O/ev7-run-$S.log 2>&1
done
echo "stream $S finished $(date -Is)" >> $O/ev7-run-$S.log
