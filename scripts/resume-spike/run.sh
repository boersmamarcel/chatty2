#!/usr/bin/env bash
# The resume spike (RC-1, AGE-650): arm R (re-brief) against arm C (cold
# resume) on the task set in tasks/, warm or cold. See README.md.
#
#   scripts/resume-spike/run.sh --provider <openrouter|ollama|fake> --model <id> \
#     --pairs N --condition warm|cold [--arm rebrief|resume|handles] [options]
#
# Results land in target/resume-spike/<run-id>/; report.py turns them into
# docs/research/resume-spike-<date>.md.
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
          \( -name '*.rs' -o -name 'Cargo.toml' -o -name 'Cargo.lock' \) -newer "$BIN" -print -quit)" ]; then
      echo "building chatty-tui --release (this takes a while)" >&2
      (cd "$ROOT" && cargo build --release -p chatty-tui)
    fi
  fi
  [ -x "$BIN" ] || { echo "resume-spike: $BIN is not an executable" >&2; exit 2; }
  extra=(--chatty-tui "$BIN")
fi

exec python3 "$ROOT/scripts/resume-spike/spike.py" "$@" ${extra[@]+"${extra[@]}"}
