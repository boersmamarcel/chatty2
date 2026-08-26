#!/usr/bin/env bash
# AGE-26: shipping crates must not depend on chatty-eval (or other eval-only crates).
set -euo pipefail
cd "$(dirname "$0")/.."

SHIPPING=(
  chatty-core
  chatty-trace
  chatty-playbook
  chatty-flow
  chatty-gpui
  chatty-tui
)

FORBIDDEN=(chatty-eval)

fail=0
for crate in "${SHIPPING[@]}"; do
  # Package may be a binary crate; -p still works for tree.
  tree=$(cargo tree -p "$crate" --prefix none --edges normal 2>/dev/null || true)
  for bad in "${FORBIDDEN[@]}"; do
    if printf '%s\n' "$tree" | grep -E "^${bad} " >/dev/null 2>&1; then
      echo "DEPENDENCY ISOLATION VIOLATION: $crate depends on $bad"
      fail=1
    fi
  done
done

if [ "$fail" -eq 0 ]; then
  echo "eval-isolation check: OK (no shipping crate depends on chatty-eval)"
fi
exit "$fail"
