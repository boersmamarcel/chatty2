#!/usr/bin/env bash
# pr-nurse-decide.sh — pure decision logic for the PR nurse's attempt budget.
#
# AGE-705: the nurse used to give up the instant its own push count reached
# NURSE_MAX_ATTEMPTS, without waiting to see whether CI on that push actually
# passed. It also never noticed when a human manually reset the budget by
# removing `blocked:human`, so the very next triage run put the label right
# back. Both bugs disarmed PRs (#926, #937) whose fix was already green.
#
# This script holds only the decision, no GitHub calls: given how many nurse
# commits are on the PR, the required-check status of the nurse's own last
# push, and whether a human removed `blocked:human` after that push, decide
# what happens next. scripts/pr-nurse-triage.sh does the GitHub calls and
# calls this for each PR that has reached the attempt cap.
#
# Env in:
#   ATTEMPTS                      - nurse commits on the PR (int)
#   MAX_ATTEMPTS                  - cap (default 2)
#   REQUIRED_CHECK_STATUS         - pending | failed | ok
#                                   (required-check rollup for the commit the
#                                   nurse's last push produced; ignored once
#                                   LABEL_REMOVED_SINCE_LAST_PUSH is true)
#   LABEL_REMOVED_SINCE_LAST_PUSH - true | false (default false) - a human
#                                   removed blocked:human after the nurse's
#                                   last "nurse:" commit
#
# Stdout: `decision=<proceed|wait|keep_armed|give_up|reset>` then
#         `reason=<why>`. Exit 0 always for a valid decision; exit 1 on bad
#         input (e.g. an unrecognised REQUIRED_CHECK_STATUS).
set -euo pipefail

ATTEMPTS="${ATTEMPTS:?ATTEMPTS is required}"
MAX_ATTEMPTS="${MAX_ATTEMPTS:-2}"
REQUIRED_CHECK_STATUS="${REQUIRED_CHECK_STATUS:-pending}"
LABEL_REMOVED_SINCE_LAST_PUSH="${LABEL_REMOVED_SINCE_LAST_PUSH:-false}"

decide() {
  local attempts="$1" max="$2" status="$3" reset="$4"

  if [ "$reset" = "true" ]; then
    echo "decision=reset"
    echo "reason=a human removed blocked:human after the nurse's last push; the attempt budget starts over"
    return
  fi

  if [ "$attempts" -lt "$max" ]; then
    echo "decision=proceed"
    echo "reason=attempts ($attempts) below the cap ($max)"
    return
  fi

  case "$status" in
    pending)
      echo "decision=wait"
      echo "reason=attempts at the cap ($attempts/$max) but CI on the nurse's own last push has not completed yet"
      ;;
    ok)
      echo "decision=keep_armed"
      echo "reason=attempts at the cap ($attempts/$max) but the nurse's own last push went green; the attempt budget clears"
      ;;
    failed)
      echo "decision=give_up"
      echo "reason=attempts at the cap ($attempts/$max) and CI on the nurse's own last push is red"
      ;;
    *)
      echo "::error::pr-nurse-decide.sh: unknown REQUIRED_CHECK_STATUS '$status' (want pending|failed|ok)" >&2
      return 1
      ;;
  esac
}

decide "$ATTEMPTS" "$MAX_ATTEMPTS" "$REQUIRED_CHECK_STATUS" "$LABEL_REMOVED_SINCE_LAST_PUSH"
