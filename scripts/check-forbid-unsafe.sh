#!/usr/bin/env bash
# AGE-26: shipping research crates forbid unsafe_code at the crate root.
set -euo pipefail
cd "$(dirname "$0")/.."

fail=0
for crate in chatty-trace chatty-playbook chatty-flow chatty-optimize; do
  lib="crates/${crate}/src/lib.rs"
  if ! grep -F '#![forbid(unsafe_code)]' "$lib" >/dev/null; then
    echo "UNSAFE VIOLATION: $lib must contain #![forbid(unsafe_code)]"
    fail=1
  fi
done

if [ "$fail" -eq 0 ]; then
  echo "forbid-unsafe check: OK"
fi
exit "$fail"
