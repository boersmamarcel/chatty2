#!/usr/bin/env bash
# AGE-26: shipping research crates (and the workspace) declare MSRV 1.85.
set -euo pipefail
cd "$(dirname "$0")/.."

EXPECTED="${CHATTY_MSRV:-1.85}"
fail=0

check_file() {
  local file=$1
  if ! grep -E "rust-version\s*=\s*\"${EXPECTED}\"" "$file" >/dev/null; then
    echo "MSRV VIOLATION: $file must set rust-version = \"${EXPECTED}\""
    fail=1
  fi
}

check_file Cargo.toml
for crate in chatty-trace chatty-playbook chatty-flow chatty-optimize; do
  check_file "crates/${crate}/Cargo.toml"
done

if [ "$fail" -eq 0 ]; then
  echo "msrv check: OK (rust-version = ${EXPECTED})"
fi
exit "$fail"
