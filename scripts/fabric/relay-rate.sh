#!/usr/bin/env bash
# The F3 gate's number (TM-3): of the multi-worker runs in the broker's edge
# logs, how many contain a relay request, a child's message to its owner that
# names a sibling.
#
# Usage: relay-rate.sh <edge-log.jsonl>...
#
# A run is one `root_task_id`. A run is multi-worker when one owner (a task
# row's `from`) started at least two distinct workers (its `to`) in it. A run
# has a relay when one of its message rows has `names_sibling: true`.
# Rows without a `root_task_id` (refusals, older logs) belong to no run.
#
# Prints:
#   runs=<n> multi_worker_runs=<n> runs_with_relay=<n> rate=<0.000>
# where rate is runs_with_relay / multi_worker_runs, 0.000 with no
# multi-worker run.
set -euo pipefail

if [ "$#" -eq 0 ]; then
  echo "usage: $0 <edge-log.jsonl>..." >&2
  exit 2
fi
for file in "$@"; do
  [ -r "$file" ] || { echo "$0: cannot read $file" >&2; exit 2; }
done

jq -rs '
  map(select(.root_task_id != null)) | group_by(.root_task_id) |
  map({
    multi: (
      [.[] | select(.kind == "task")] | group_by(.from)
      | any(.[]; (map(.to) | unique | length) >= 2)
    ),
    relay: any(.[]; .kind == "message" and .names_sibling == true)
  }) |
  [length, (map(select(.multi)) | length), (map(select(.multi and .relay)) | length)] | @tsv
' "$@" | LC_ALL=C awk -F'\t' '{
  rate = ($2 == 0) ? 0 : $3 / $2
  printf "runs=%d multi_worker_runs=%d runs_with_relay=%d rate=%.3f\n", $1, $2, $3, rate
}'
