#!/usr/bin/env bash
# Decision-table tests for scripts/pr-nurse-decide.sh (AGE-705).
# Invoked from CI (see ci.yml's `changes` job) and safe to run directly.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="$ROOT/scripts/pr-nurse-decide.sh"
fail=0

run_decide() {
  ATTEMPTS="${ATTEMPTS:?}" \
    MAX_ATTEMPTS="${MAX_ATTEMPTS:-2}" \
    REQUIRED_CHECK_STATUS="${REQUIRED_CHECK_STATUS:-pending}" \
    LABEL_REMOVED_SINCE_LAST_PUSH="${LABEL_REMOVED_SINCE_LAST_PUSH:-false}" \
    bash "$SCRIPT"
}

expect_decision() {
  local desc="$1" want="$2" got
  if ! got="$(run_decide)"; then
    echo "FAIL: $desc (script exited non-zero)"
    fail=1
    return
  fi
  if ! grep -qx "decision=$want" <<<"$got"; then
    echo "FAIL: $desc — expected decision=$want, got:"
    echo "$got"
    fail=1
    return
  fi
  echo "OK: $desc ($got)"
}

echo "Running pr-nurse-decide decision-table tests"

# nurse_waits_for_ci_on_its_last_push:
# Attempt 2 just pushed; CI on that head hasn't reported yet. Do not give up
# out from under a push that might still go green (the #937/#926 bug: the
# old triage script gave up ~20s after the 2nd push, before CI ran at all).
ATTEMPTS=2 MAX_ATTEMPTS=2 REQUIRED_CHECK_STATUS=pending LABEL_REMOVED_SINCE_LAST_PUSH=false \
  expect_decision "nurse_waits_for_ci_on_its_last_push" "wait"

# nurse_keeps_armed_when_its_fix_goes_green:
# Attempt 2's CI finished green. Auto-merge stays armed; the attempt budget
# clears instead of staying permanently exhausted.
ATTEMPTS=2 MAX_ATTEMPTS=2 REQUIRED_CHECK_STATUS=ok LABEL_REMOVED_SINCE_LAST_PUSH=false \
  expect_decision "nurse_keeps_armed_when_its_fix_goes_green" "keep_armed"

# nurse_gives_up_when_its_fix_goes_red:
# Attempt 2's CI finished red: no attempts left, give up as before.
ATTEMPTS=2 MAX_ATTEMPTS=2 REQUIRED_CHECK_STATUS=failed LABEL_REMOVED_SINCE_LAST_PUSH=false \
  expect_decision "nurse_gives_up_when_its_fix_goes_red" "give_up"

# manual_label_removal_resets_budget:
# A human removed blocked:human after the nurse's last push. The next triage
# run must not immediately re-disarm and re-label just because the historical
# attempt count is still at the cap (the second half of the #937 bug: Marcel
# removed the label and the nurse put it right back ~14s later).
ATTEMPTS=2 MAX_ATTEMPTS=2 REQUIRED_CHECK_STATUS=failed LABEL_REMOVED_SINCE_LAST_PUSH=true \
  expect_decision "manual_label_removal_resets_budget" "reset"

# Below the cap: normal triage proceeds regardless of check status.
ATTEMPTS=1 MAX_ATTEMPTS=2 REQUIRED_CHECK_STATUS=pending LABEL_REMOVED_SINCE_LAST_PUSH=false \
  expect_decision "below cap proceeds unaffected" "proceed"

# Reset wins even below the cap (a human's removal always takes priority).
ATTEMPTS=0 MAX_ATTEMPTS=2 REQUIRED_CHECK_STATUS=ok LABEL_REMOVED_SINCE_LAST_PUSH=true \
  expect_decision "reset takes priority over an already-clear budget" "reset"

if [ "$fail" -ne 0 ]; then
  echo "pr-nurse-decide tests: FAILED"
  exit 1
fi
echo "pr-nurse-decide tests: OK"
