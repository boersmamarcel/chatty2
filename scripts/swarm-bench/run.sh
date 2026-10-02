#!/usr/bin/env bash
# The swarm-vs-single benchmark (EV-3, AGE-670): each task run by one harness
# agent (arm `single`) and by its family's frozen team preset (arm `swarm`)
# on the same model. The protocol is docs/research/swarm-vs-single-prereg.md;
# see README.md.
#
#   scripts/swarm-bench/run.sh --provider <openai-compat|fake> --model <id> \
#     [--arm single|swarm|both] [--think false] [--run-id ID] [options]
#
# Results land in target/swarm-bench/<run-id>/ (re-running with the same
# run id resumes); report.py turns them into
# docs/research/swarm-vs-single-<date>.md.
#
#   CHATTY_TUI=<path>   use this binary instead of target/release/chatty-tui
#                       (--chatty-tui <path> does the same)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"

have_bin=0
for arg in "$@"; do
  case "$arg" in --chatty-tui|-h|--help) have_bin=1 ;; esac
done

extra=()
if [ "$have_bin" = 0 ]; then
  if [ -n "${CHATTY_TUI:-}" ]; then
    BIN="$CHATTY_TUI"
  else
    BIN="${CARGO_TARGET_DIR:-$ROOT/target}/release/chatty-tui"
    if [ ! -x "$BIN" ] || [ -n "$(find "$ROOT/crates" "$ROOT/Cargo.toml" "$ROOT/Cargo.lock" \
          \( -name '*.rs' -o -name 'Cargo.toml' -o -name 'Cargo.lock' -o -name '*.toml' -o -name '*.json' \) \
          -newer "$BIN" -print -quit)" ]; then
      echo "building chatty-tui --release (this takes a while)" >&2
      (cd "$ROOT" && cargo build --release -p chatty-tui)
    fi
  fi
  [ -x "$BIN" ] || { echo "swarm-bench: $BIN is not an executable" >&2; exit 2; }
  extra=(--chatty-tui "$BIN")
fi

exec python3 "$ROOT/scripts/swarm-bench/bench.py" "$@" ${extra[@]+"${extra[@]}"}
