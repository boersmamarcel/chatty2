#!/usr/bin/env bash
# One-lane merge queue for a personal-account repo (GitHub's queue is
# organization-only). Finds the front of the queue and presses "update branch"
# on it, so the strict up-to-date policy on main never needs a human.
#
# Queue = open, non-draft, same-repo PRs with auto-merge armed, oldest first.
# Walk it front to back:
#   DIRTY (conflict)            -> skip: the nurse's job, must not block the lane
#   required check FAILED       -> skip: stalled, the nurse's job
#   BEHIND                      -> update this one and stop (one lane)
#   anything else               -> its head is current and checks are running or
#                                  green; auto-merge will land it. Stop.
#
# Usage: update-next-queued-pr.sh <owner/repo> [--dry-run]
# Needs GH_TOKEN with contents + pull-requests write (a fine-grained PAT, not
# GITHUB_TOKEN: its branch updates do not trigger CI on the PR).
set -euo pipefail

REPO="${1:?usage: $0 <owner/repo> [--dry-run]}"
DRY_RUN="${2:-}"
OWNER="${REPO%%/*}"
REQUIRED='["test","check-reserved","guard"]'

queue="$(gh pr list --repo "$REPO" --state open --limit 100 \
  --json number,isDraft,createdAt,autoMergeRequest,headRepositoryOwner \
  --jq '[.[] | select(.autoMergeRequest != null and .isDraft == false
                      and .headRepositoryOwner.login == "'"$OWNER"'")]
         | sort_by(.createdAt) | map(.number) | .[]')"

if [ -z "$queue" ]; then
  echo "Queue is empty; nothing to do."
  exit 0
fi
echo "Queue (oldest first): $(echo "$queue" | tr '\n' ' ')"

pr_state() {
  # The list endpoint often reports UNKNOWN; the single-PR endpoint triggers
  # GitHub's mergeability computation. Retry briefly.
  local n="$1" s
  for _ in 1 2 3 4; do
    s="$(gh pr view "$n" --repo "$REPO" --json mergeStateStatus --jq '.mergeStateStatus')"
    [ "$s" != "UNKNOWN" ] && { echo "$s"; return; }
    sleep 5
  done
  echo "$s"
}

pr_required_failed() {
  gh pr view "$1" --repo "$REPO" --json statusCheckRollup \
    --jq '[.statusCheckRollup[]? | select((.name // .context) as $n | '"$REQUIRED"' | index($n))
           | .conclusion // .state]
          | map(select(. == "FAILURE" or . == "CANCELLED" or . == "TIMED_OUT" or . == "ERROR"))
          | length > 0'
}

for n in $queue; do
  state="$(pr_state "$n")"
  case "$state" in
    DIRTY)
      echo "PR #$n: conflicts with main; skipping (nurse)."
      continue
      ;;
    BEHIND)
      if [ "$DRY_RUN" = "--dry-run" ]; then
        echo "PR #$n: BEHIND; would update branch (dry run)."
      else
        gh api --method PUT "repos/$REPO/pulls/$n/update-branch" >/dev/null
        echo "PR #$n: BEHIND; updated. CI reruns on the new head; auto-merge lands it when green."
      fi
      exit 0
      ;;
    *)
      if [ "$(pr_required_failed "$n")" = "true" ]; then
        echo "PR #$n: $state with a failed required check; skipping (nurse)."
        continue
      fi
      echo "PR #$n: $state, head is current and checks are pending or green; lane busy. Nothing to update."
      exit 0
      ;;
  esac
done
echo "Every queued PR is conflicted or failing; nothing to update."
