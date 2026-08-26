#!/usr/bin/env bash
# check-release-authz.sh — assert closed-system release authz gates exist in workflows.
#
# Threat model: only github.repository_owner and github-actions[bot] may arm
# releases; fork / outsider PRs must not. Run locally or from CI:
#
#   bash scripts/check-release-authz.sh
#
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WF="$ROOT/.github/workflows"
fail=0

need() {
  local file="$1"
  local pattern="$2"
  local desc="$3"
  if ! grep -Eq "$pattern" "$file"; then
    echo "FAIL: $desc"
    echo "  file: $file"
    echo "  expected pattern: $pattern"
    fail=1
  else
    echo "OK: $desc"
  fi
}

echo "Checking release authz gates under .github/workflows/"

need "$WF/release.yml" \
  'github\.actor == github\.repository_owner' \
  "release.yml gates workflow_dispatch to repository_owner"

need "$WF/release.yml" \
  'refs/tags/' \
  "release.yml checks out immutable refs/tags/ refs"

need "$WF/release.yml" \
  'merge-base --is-ancestor' \
  "release.yml asserts tag commit is ancestor of origin/main"

need "$WF/release.yml" \
  'git_ref' \
  "release.yml exports git_ref for checkout steps"

need "$WF/prepare-release.yml" \
  'github\.actor == github\.repository_owner' \
  "prepare-release.yml gates workflow_dispatch to repository_owner"

need "$WF/prepare-release.yml" \
  'head\.repo\.full_name == github\.repository' \
  "prepare-release.yml requires same-repo PR head"

need "$WF/ship-auto-merge.yml" \
  'head\.repo\.full_name|HEAD_REPO' \
  "ship-auto-merge.yml requires same-repo head"

need "$WF/ship-auto-merge.yml" \
  'github-actions\[bot\]' \
  "ship-auto-merge.yml allows github-actions[bot] actor"

need "$WF/ship-auto-merge.yml" \
  'repository_owner|OWNER' \
  "ship-auto-merge.yml requires repository_owner sender"

need "$WF/ship-auto-guard.yml" \
  'release:\(patch\|minor\|major\)|release:patch' \
  "ship-auto-guard.yml keys off release labels"

need "$WF/privileged-labels.yml" \
  'pull-requests:\s*write' \
  "privileged-labels.yml has pull-requests: write"

need "$WF/privileged-labels.yml" \
  'github-actions\[bot\]' \
  "privileged-labels.yml allows github-actions[bot]"

need "$WF/privileged-labels.yml" \
  'remove-label|Remove privileged label' \
  "privileged-labels.yml removes unauthorized privileged labels"

if [ "$fail" -ne 0 ]; then
  echo ""
  echo "check-release-authz.sh: FAILED"
  exit 1
fi

echo ""
echo "check-release-authz.sh: OK"
