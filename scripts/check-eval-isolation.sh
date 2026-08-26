#!/usr/bin/env bash
# AGE-26 remnant: ensure the deleted chatty-eval crate is not reintroduced as a
# dependency of shipping crates, and that Harbor owns Stage B sandboxes.
set -euo pipefail
cd "$(dirname "$0")/.."

if [ -d crates/chatty-eval ]; then
  echo "ISOLATION VIOLATION: crates/chatty-eval exists — Stage B is Harbor (AGE-34); delete this crate"
  exit 1
fi

SHIPPING=(
  chatty-core
  chatty-trace
  chatty-playbook
  chatty-flow
  chatty-gpui
  chatty-tui
)

fail=0
for crate in "${SHIPPING[@]}"; do
  tree=$(cargo tree -p "$crate" --prefix none --edges normal 2>/dev/null || true)
  if printf '%s\n' "$tree" | grep -E "^chatty-eval " >/dev/null 2>&1; then
    echo "DEPENDENCY ISOLATION VIOLATION: $crate depends on chatty-eval"
    fail=1
  fi
done

if [ "$fail" -eq 0 ]; then
  echo "eval-isolation check: OK (no chatty-eval crate; Harbor owns Stage B)"
fi
exit "$fail"
