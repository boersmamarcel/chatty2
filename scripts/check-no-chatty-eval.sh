#!/usr/bin/env bash
# AGE-24 / AGE-26: refuse a resurrected chatty-eval crate.
# Stage B sandboxes live in harbor-chatty (AGE-34); optimizer helpers live in chatty-optimize.
set -euo pipefail
cd "$(dirname "$0")/.."

fail=0

if [ -d crates/chatty-eval ] || [ -f crates/chatty-eval/Cargo.toml ]; then
  echo "CHATTY-EVAL VIOLATION: crates/chatty-eval must not exist"
  echo "  Stage B → harbor-chatty (AGE-34); paired stats / QA loaders → chatty-optimize"
  fail=1
fi

if grep -E '^\s*"crates/chatty-eval"' Cargo.toml >/dev/null 2>&1; then
  echo "CHATTY-EVAL VIOLATION: workspace members list crates/chatty-eval"
  fail=1
fi

if [ "$fail" -eq 0 ]; then
  echo "chatty-eval check: OK (absent)"
fi
exit "$fail"
