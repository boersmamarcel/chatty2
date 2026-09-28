#!/usr/bin/env bash
# PR nurse triage: which PR needs care, and why. No AI here.
#
# In care = open, not draft, same-repo head, auto-merge armed, not labelled
# blocked:human. Oldest first. For each:
#   nurse attempts >= NURSE_MAX_ATTEMPTS  -> ask scripts/pr-nurse-decide.sh
#                                            (AGE-705: only give up once CI on
#                                            the nurse's own last push has
#                                            actually reported red; a human
#                                            removing blocked:human resets the
#                                            budget instead of it staying
#                                            permanently exhausted)
#   mergeStateStatus DIRTY                -> conflict
#   a required check FAILED on the head   -> ci-failed
#   a required check still pending        -> wait (a push just happened)
#   otherwise                             -> nothing; auto-merge lands it
# Prints JSON: {"pick": {"number", "reason", "state"} | null, "gave_up": [n...]}
#
# Usage: pr-nurse-triage.sh <owner/repo> [pr-number]
#        With a PR number, only that PR is considered (workflow_dispatch).
# Env: REQUIRED_CHECKS (comma list, default test,check-reserved,guard)
#      NURSE_MAX_ATTEMPTS (default 2)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="${1:?usage: $0 <owner/repo> [pr-number]}"
ONLY="${2:-}"
OWNER="${REPO%%/*}"
REQUIRED="${REQUIRED_CHECKS:-test,check-reserved,guard}"
MAX="${NURSE_MAX_ATTEMPTS:-2}"
REQ_JSON="$(printf '%s' "$REQUIRED" | jq -R 'split(",")')"

queue="$(gh pr list --repo "$REPO" --state open --limit 100 \
  --json number,isDraft,createdAt,autoMergeRequest,headRepositoryOwner,labels \
  --jq '[.[] | select(.autoMergeRequest != null and .isDraft == false
                      and .headRepositoryOwner.login == "'"$OWNER"'"
                      and (([.labels[].name] | index("blocked:human")) == null))]
         | sort_by(.createdAt) | map(.number) | .[]')"
if [ -n "$ONLY" ]; then
  queue="$(grep -x "$ONLY" <<<"$queue" || true)"
  [ -z "$queue" ] && echo "PR #$ONLY is not in care (open, non-draft, same-repo, auto-merge armed, not blocked:human)." >&2
fi

pr_state() {
  local n="$1" s
  for _ in 1 2 3 4; do
    s="$(gh pr view "$n" --repo "$REPO" --json mergeStateStatus --jq '.mergeStateStatus')"
    [ "$s" != "UNKNOWN" ] && { echo "$s"; return; }
    sleep 5
  done
  echo "$s"
}

# "failed" | "pending" | "ok" for the required checks on the head commit.
pr_required() {
  gh pr view "$1" --repo "$REPO" --json statusCheckRollup \
    --jq '[.statusCheckRollup[]? | select((.name // .context) as $n | '"$REQ_JSON"' | index($n))
           | (.conclusion // .state // "PENDING")]
          | if any(. == "FAILURE" or . == "CANCELLED" or . == "TIMED_OUT" or . == "ERROR") then "failed"
            elif any(. == "PENDING" or . == "IN_PROGRESS" or . == "QUEUED" or . == "EXPECTED" or . == "") then "pending"
            elif length == 0 then "pending"
            else "ok" end'
}

pr_attempts() {
  gh pr view "$1" --repo "$REPO" --json commits \
    --jq '[.commits[] | select(.messageHeadline | startswith("nurse:"))] | length'
}

pr_last_nurse_push_time() {
  gh pr view "$1" --repo "$REPO" --json commits \
    --jq '[.commits[] | select(.messageHeadline | startswith("nurse:")) | .committedDate] | sort | last // empty'
}

# Did a human remove blocked:human after the nurse's own last push? That is
# the signal that the attempt budget was manually reset (AGE-705): otherwise
# the very next triage run sees the same exhausted attempt count and puts the
# label right back within seconds.
pr_label_removed_since_last_push() {
  local n="$1" last_push removed
  last_push="$(pr_last_nurse_push_time "$n")"
  [ -n "$last_push" ] || { echo false; return; }
  removed="$(gh api "repos/$REPO/issues/$n/timeline" --paginate \
    --jq '[.[] | select(.event == "unlabeled" and .label.name == "blocked:human") | .created_at] | sort | last // empty')"
  if [ -n "$removed" ] && [[ "$removed" > "$last_push" ]]; then
    echo true
  else
    echo false
  fi
}

pick="null"
gave_up="[]"
for n in $queue; do
  attempts="$(pr_attempts "$n")"
  if [ "$attempts" -ge "$MAX" ]; then
    own_push_status="$(pr_required "$n")"
    label_removed="$(pr_label_removed_since_last_push "$n")"
    decision_out="$(ATTEMPTS="$attempts" MAX_ATTEMPTS="$MAX" \
      REQUIRED_CHECK_STATUS="$own_push_status" \
      LABEL_REMOVED_SINCE_LAST_PUSH="$label_removed" \
      bash "$SCRIPT_DIR/pr-nurse-decide.sh")"
    decision="$(grep '^decision=' <<<"$decision_out" | cut -d= -f2)"
    reason="$(grep '^reason=' <<<"$decision_out" | cut -d= -f2-)"
    case "$decision" in
      wait)
        echo "PR #$n: $reason" >&2
        continue
        ;;
      give_up)
        echo "PR #$n: $reason" >&2
        gave_up="$(jq -c ". + [$n]" <<<"$gave_up")"
        continue
        ;;
      keep_armed | reset)
        echo "PR #$n: $reason" >&2
        # Fall through to the normal state/required-check triage below
        # instead of giving up; a fresh conflict or failure gets a full
        # budget again.
        ;;
      *)
        echo "PR #$n: pr-nurse-decide.sh returned an unrecognised decision '$decision'; treating as give_up." >&2
        gave_up="$(jq -c ". + [$n]" <<<"$gave_up")"
        continue
        ;;
    esac
  fi
  state="$(pr_state "$n")"
  if [ "$state" = "DIRTY" ]; then
    echo "PR #$n: conflict with main." >&2
    [ "$pick" = "null" ] && pick="$(jq -cn --argjson n "$n" --arg s "$state" '{number:$n, reason:"conflict", state:$s}')"
    continue
  fi
  req="$(pr_required "$n")"
  case "$req" in
    failed)
      echo "PR #$n: a required check failed on the head." >&2
      [ "$pick" = "null" ] && pick="$(jq -cn --argjson n "$n" --arg s "$state" '{number:$n, reason:"ci-failed", state:$s}')"
      ;;
    pending) echo "PR #$n: checks pending ($state); leave it." >&2 ;;
    ok)      echo "PR #$n: green ($state); the queue and auto-merge own it." >&2 ;;
  esac
done

jq -cn --argjson pick "$pick" --argjson gave_up "$gave_up" '{pick:$pick, gave_up:$gave_up}'
