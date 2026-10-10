#!/bin/bash
# AGE-862: (re)start the meter, the eval queue and the proposer driver. Idempotent.
R=/media/marcel/data/rust/swarm-results/age-862
PY=/media/marcel/data/rust/swarm-results/ev7-venv/bin/python
S=$R/wt/scripts/meta-harness
cd $R
pgrep -f "$R/meter.sh" >/dev/null || setsid nohup bash $R/meter.sh >/dev/null 2>&1 < /dev/null &
pgrep -f "$S/mh.py queue" >/dev/null || setsid nohup $PY $S/mh.py queue >> $R/queue.out 2>&1 < /dev/null &
[ "${1:-}" = "--no-proposer" ] || pgrep -f "$S/proposer.py" >/dev/null || setsid nohup $PY $S/proposer.py --max-iters 20 --pilot 5 >> $R/proposer.out 2>&1 < /dev/null &
sleep 2; pgrep -af "meter.sh|mh.py queue|proposer.py" | grep -v pgrep
