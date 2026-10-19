#!/usr/bin/env bash
# AGE-116: fail when docs/INDEX.md or docs-site/src/SUMMARY.md drift from repo markdown.
set -euo pipefail
cd "$(dirname "$0")/.."

fail=0
INDEX="docs/INDEX.md"
SUMMARY="docs-site/src/SUMMARY.md"

if [[ ! -f "$INDEX" ]]; then
  echo "missing $INDEX"
  exit 1
fi
if [[ ! -f "$SUMMARY" ]]; then
  echo "missing $SUMMARY"
  exit 1
fi

# docs-sync must run first so docs-site/src reflects docs/ + crate READMEs.
bash scripts/docs-sync.sh >/dev/null

check_index() {
  local missing=0
  while IFS= read -r -d '' f; do
    rel="${f#docs/}"
    # INDEX lists paths like `foo.md` or `research/bar.md`
    if ! grep -qF "$rel" "$INDEX"; then
      echo "INDEX drift: docs/$rel not listed in $INDEX"
      missing=$((missing + 1))
    fi
  done < <(find docs -name '*.md' \
    ! -path 'docs/generated/*' \
    ! -name 'INDEX.md' \
    -print0)

  if [[ "$missing" -gt 0 ]]; then
    fail=1
    echo "  → add a row to docs/INDEX.md for each file above"
  else
    echo "INDEX nav check: OK"
  fi
}

check_summary() {
  local missing=0
  while IFS= read -r -d '' f; do
    base="${f#docs-site/src/}"
    [[ "$base" == "SUMMARY.md" ]] && continue
    # SUMMARY uses mdBook paths like ./dev/agents.md
    if ! grep -qF "./${base}" "$SUMMARY"; then
      echo "SUMMARY drift: docs-site/src/$base not linked in $SUMMARY"
      missing=$((missing + 1))
    fi
  done < <(find docs-site/src -name '*.md' -print0)

  if [[ "$missing" -gt 0 ]]; then
    fail=1
    echo "  → add an entry to docs-site/src/SUMMARY.md for each file above"
  else
    echo "SUMMARY nav check: OK"
  fi
}

check_index
check_summary
exit "$fail"
