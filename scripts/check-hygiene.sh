#!/usr/bin/env bash
# Keeps files with no product value from piling up again.
#
#   1. Unreferenced files: every script (or script folder) under scripts/, every
#      GIF in assets/animations/ and every screenshot in assets/screenshots/
#      must be named by some other tracked file (a workflow, the Makefile, code
#      or a doc). Files in the repository root must be on the list below.
#   2. Suppressions need a reason: no `allow(dead_code)` without `reason = ...`
#      (prefer `#[expect(dead_code, reason = ...)]`), and no bare `#[ignore]`.
#
# Exceptions go in scripts/hygiene-allow.txt, one path per line with a comment
# saying why. Unused Cargo dependencies are checked separately (`make machete`).
set -uo pipefail
cd "$(dirname "$0")/.."

fail=0
allow_file=scripts/hygiene-allow.txt
allowed() {
  [ -f "$allow_file" ] && grep -v '^\s*#' "$allow_file" | sed 's/\s*#.*$//' | grep -qxF "$1"
}

# Is NAME mentioned in a tracked file outside UNIT (a file or a directory)?
referenced() {
  local name="$1" unit="$2"
  git grep -q -F -e "$name" -- . ":(exclude)$unit" ":(exclude)$unit/**"
}

# 1a. scripts/: a top-level file, or a top-level folder as one unit.
while IFS= read -r unit; do
  allowed "$unit" && continue
  if ! referenced "$(basename "$unit")" "$unit"; then
    echo "::error file=$unit::nothing references $unit (wire it up, delete it, or list it in $allow_file)"
    fail=1
  fi
done < <(git ls-files scripts | awk -F/ '{ print (NF > 2) ? $1 "/" $2 : $0 }' | sort -u)

# 1b. assets that are only ever referenced by name.
while IFS= read -r f; do
  allowed "$f" && continue
  if ! referenced "$(basename "$f")" "$f"; then
    echo "::error file=$f::nothing references $f"
    fail=1
  fi
done < <(git ls-files 'assets/animations/*' 'assets/screenshots/*')

# 1c. The repository root holds only these files.
root_ok=" .gitattributes .gitignore .lychee.toml AGENTS.md CLAUDE.md CONTRIBUTING.md Cargo.lock Cargo.toml LICENSE Makefile README.md RESERVED.md deny.toml skills-lock.json "
while IFS= read -r f; do
  case "$root_ok" in *" $f "*) continue ;; esac
  allowed "$f" && continue
  echo "::error file=$f::unexpected file in the repository root: $f"
  fail=1
done < <(git ls-files | awk -F/ 'NF == 1')

# 2. Suppressions without a reason.
bare_allow=$(git grep -n -E 'allow\([^)]*dead_code' -- 'crates/**/*.rs' 'modules/**/*.rs' | grep -v 'reason *=' || true)
if [ -n "$bare_allow" ]; then
  echo "::error::allow(dead_code) without a reason; use #[expect(dead_code, reason = \"...\")] or delete the code:"
  echo "$bare_allow"
  fail=1
fi
bare_ignore=$(git grep -n -E '#\[ignore\]' -- 'crates/**/*.rs' 'modules/**/*.rs' | grep -v '^\S*:[0-9]*:\s*//' || true)
if [ -n "$bare_ignore" ]; then
  echo "::error::#[ignore] without a reason; write #[ignore = \"why\"]:"
  echo "$bare_ignore"
  fail=1
fi

if [ "$fail" -eq 0 ]; then
  echo "hygiene check: OK"
fi
exit "$fail"
