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
  "release.yml exports git_ref for tag uploads / immutable refs"

need "$WF/release.yml" \
  'from_prepare' \
  "release.yml distinguishes prepare-release reusable calls from direct dispatch"

need "$WF/release.yml" \
  'inputs.from_prepare == true' \
  "release.yml job if allows prepare-release reusable calls (inherited event_name)"

need "$WF/release.yml" \
  'scripts/release-resolve-tag.sh' \
  "release.yml resolves tags via scripts/release-resolve-tag.sh"

need "$WF/release.yml" \
  'checkout_mode == .git_ref. && needs.validate-version.outputs.git_ref' \
  "release.yml build jobs check out git_ref when mode is git_ref (not inherited event_name)"

need "$WF/prepare-release.yml" \
  'from_prepare:' \
  "prepare-release.yml passes from_prepare=true into release.yml"

need "$WF/prepare-release.yml" \
  'github\.actor == github\.repository_owner' \
  "prepare-release.yml gates workflow_dispatch to repository_owner"

need "$WF/prepare-release.yml" \
  "github\.actor == 'github-actions\[bot\]'" \
  "prepare-release.yml allows github-actions[bot] workflow_dispatch after ship-auto merge"

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

need "$WF/ship-auto-merge.yml" \
  'actions:[[:space:]]*write' \
  "ship-auto-merge.yml has actions: write so GITHUB_TOKEN can dispatch prepare-release"

need "$WF/ship-auto-merge.yml" \
  'workflow run prepare-release.yml' \
  "ship-auto-merge.yml dispatches prepare-release after an Actions squash"

need "$WF/ship-auto-merge.yml" \
  'autoMergeRequest' \
  "ship-auto-merge.yml continues the waiter when auto-merge is already armed"

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

need "$ROOT/scripts/release-resolve-tag.sh" \
  'workflow_dispatch must target the tag ref|MODE="workflow_sha"' \
  "release-resolve-tag.sh does not check out workflow_dispatch inputs as refs"

if grep -Fq "github.event_name == 'workflow_dispatch' && github.sha" "$WF/release.yml"; then
  echo "FAIL: release.yml must not key checkout on inherited github.event_name == workflow_dispatch"
  fail=1
else
  echo "OK: release.yml does not key checkout on inherited workflow_dispatch event_name"
fi

echo ""
if ! bash "$ROOT/scripts/release-resolve-tag-test.sh"; then
  fail=1
fi

if [ "$fail" -ne 0 ]; then
  echo ""
  echo "check-release-authz.sh: FAILED"
  exit 1
fi

echo ""
echo "check-release-authz.sh: OK"
