#!/usr/bin/env bash
# AGE-265: user-guide pages must not carry contributor material.
# Fails when docs-site/src/user/**.md mentions ticket IDs, crate paths,
# Rust source files, GPUI internals, or maintainer notes.
set -euo pipefail
cd "$(dirname "$0")/.."

patterns=(
  'AGE-[0-9]+'
  'crates/'
  '\.rs\b'
  'masked_env'
  '\bcx\.'
  'not inlined here'
  'CLAUDE\.md'
  'AGENTS\.md'
)

fail=0
for p in "${patterns[@]}"; do
  if hits=$(grep -rnE --include='*.md' -- "$p" docs-site/src/user); then
    echo "user-doc leakage: pattern '$p'"
    echo "$hits"
    fail=1
  fi
done

if [[ "$fail" -ne 0 ]]; then
  echo "  → user guides describe behaviour, not implementation; move this to the developer guide"
  exit 1
fi
echo "user-doc leakage check: OK"
