#!/usr/bin/env bash
# Behavioral tests for scripts/release-resolve-tag.sh.
# Invoked from check-release-authz.sh (and safe to run directly).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="$ROOT/scripts/release-resolve-tag.sh"
fail=0

run_resolve() {
  local out
  out="$(mktemp)"
  # shellcheck disable=SC2034
  if GITHUB_OUTPUT="$out" \
    EVENT_NAME="${EVENT_NAME:-}" \
    REF_TYPE="${REF_TYPE:-}" \
    REF_NAME="${REF_NAME:-}" \
    INPUT_TAG="${INPUT_TAG:-}" \
    RELEASE_TAG="${RELEASE_TAG:-}" \
    FROM_PREPARE="${FROM_PREPARE:-false}" \
    bash "$SCRIPT" >/dev/null; then
    cat "$out"
    rm -f "$out"
    return 0
  fi
  local status=$?
  rm -f "$out"
  return "$status"
}

expect_ok() {
  local desc="$1"
  local want_tag="$2"
  local want_mode="$3"
  local got
  if ! got="$(run_resolve)"; then
    echo "FAIL: $desc (expected success)"
    fail=1
    return
  fi
  if ! grep -qx "tag=$want_tag" <<<"$got"; then
    echo "FAIL: $desc — expected tag=$want_tag"
    echo "$got"
    fail=1
    return
  fi
  if ! grep -qx "checkout_mode=$want_mode" <<<"$got"; then
    echo "FAIL: $desc — expected checkout_mode=$want_mode"
    echo "$got"
    fail=1
    return
  fi
  if ! grep -qx "git_ref=refs/tags/$want_tag" <<<"$got"; then
    echo "FAIL: $desc — expected git_ref=refs/tags/$want_tag"
    echo "$got"
    fail=1
    return
  fi
  echo "OK: $desc"
}

expect_fail() {
  local desc="$1"
  local needle="$2"
  local err outgh
  err="$(mktemp)"
  outgh="$(mktemp)"
  if GITHUB_OUTPUT="$outgh" \
    EVENT_NAME="${EVENT_NAME:-}" \
    REF_TYPE="${REF_TYPE:-}" \
    REF_NAME="${REF_NAME:-}" \
    INPUT_TAG="${INPUT_TAG:-}" \
    RELEASE_TAG="${RELEASE_TAG:-}" \
    FROM_PREPARE="${FROM_PREPARE:-false}" \
    bash "$SCRIPT" >"$err" 2>&1; then
    echo "FAIL: $desc (expected failure)"
    rm -f "$err" "$outgh"
    fail=1
    return
  fi
  if ! grep -q "$needle" "$err"; then
    echo "FAIL: $desc — output did not contain: $needle"
    cat "$err"
    rm -f "$err" "$outgh"
    fail=1
    return
  fi
  rm -f "$err" "$outgh"
  echo "OK: $desc"
}

echo "Running release-resolve-tag decision-table tests"

# Production failure: prepare-release dispatched from main (v0.3.32–v0.3.34).
EVENT_NAME=workflow_dispatch REF_TYPE=branch REF_NAME=main \
  INPUT_TAG=v0.3.34 FROM_PREPARE=true \
  expect_ok "prepare-release dispatch inherits workflow_dispatch" "v0.3.34" "git_ref"

# Production silent-skip sibling: same call with from_prepare, actor is bot.
# (Actor is a job-level if; the script must still resolve the tag.)
EVENT_NAME=workflow_dispatch REF_TYPE=branch REF_NAME=main \
  INPUT_TAG=v0.3.31 FROM_PREPARE=true \
  expect_ok "prepare-release dispatch from github-actions[bot]" "v0.3.31" "git_ref"

# Last successful artifact path: prepare-release on pull_request.closed.
EVENT_NAME=pull_request REF_TYPE=branch REF_NAME=main \
  INPUT_TAG=v0.3.30 FROM_PREPARE=true \
  expect_ok "prepare-release pull_request.closed reusable call" "v0.3.30" "git_ref"

# Rare: GitHub sets event_name to workflow_call.
EVENT_NAME=workflow_call REF_TYPE=branch REF_NAME=main \
  INPUT_TAG=v0.3.30 FROM_PREPARE=false \
  expect_ok "event_name=workflow_call without from_prepare" "v0.3.30" "git_ref"

# Direct emergency rebuild against the tag (AGE-35).
EVENT_NAME=workflow_dispatch REF_TYPE=tag REF_NAME=v0.3.34 \
  INPUT_TAG=v0.3.34 FROM_PREPARE=false \
  expect_ok "direct release.yml dispatch from tag ref" "v0.3.34" "workflow_sha"

# Direct dispatch from main must still fail (CodeQL: do not checkout the input).
EVENT_NAME=workflow_dispatch REF_TYPE=branch REF_NAME=main \
  INPUT_TAG=v0.3.34 FROM_PREPARE=false \
  expect_fail "direct dispatch from branch is forbidden" "must target the tag ref"

# Mismatched dispatch input vs tag ref.
EVENT_NAME=workflow_dispatch REF_TYPE=tag REF_NAME=v0.3.34 \
  INPUT_TAG=v0.3.33 FROM_PREPARE=false \
  expect_fail "dispatch tag_name must match tag ref" "must match dispatch tag ref"

# release.published fallback.
EVENT_NAME=release REF_TYPE=tag REF_NAME=v0.3.28 \
  RELEASE_TAG=v0.3.28 FROM_PREPARE=false \
  expect_ok "release.published event" "v0.3.28" "git_ref"

# Empty tag.
EVENT_NAME=release FROM_PREPARE=false RELEASE_TAG= \
  expect_fail "empty tag is rejected" "No tag name provided"

# Invalid format.
EVENT_NAME=workflow_call INPUT_TAG=not-a-version FROM_PREPARE=true \
  expect_fail "invalid tag format is rejected" "Invalid tag format"

if [ "$fail" -ne 0 ]; then
  echo "release-resolve-tag tests: FAILED"
  exit 1
fi
echo "release-resolve-tag tests: OK"
