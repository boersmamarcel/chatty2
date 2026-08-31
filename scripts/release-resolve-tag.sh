#!/usr/bin/env bash
# release-resolve-tag.sh — decide tag + checkout mode for release.yml.
#
# Reusable workflows inherit the caller's github.event_name (actions/runner#3146).
# A prepare-release.yml workflow_dispatch therefore arrives here as
# EVENT_NAME=workflow_dispatch REF_TYPE=branch, which must NOT be treated as a
# direct emergency rebuild of release.yml. FROM_PREPARE=true is the discriminator
# (workflow_call-only input; not a workflow_dispatch input, so operators cannot
# set it to bypass the tag-ref requirement).
#
# Writes checkout_mode, tag, git_ref to $GITHUB_OUTPUT when that file is set.
# Also prints "Resolved tag: … (mode=…)" for logs.
set -euo pipefail

EVENT_NAME="${EVENT_NAME:-}"
REF_TYPE="${REF_TYPE:-}"
REF_NAME="${REF_NAME:-}"
INPUT_TAG="${INPUT_TAG:-}"
RELEASE_TAG="${RELEASE_TAG:-}"
FROM_PREPARE="${FROM_PREPARE:-false}"

if [ "$FROM_PREPARE" = "true" ] || [ "$EVENT_NAME" = "workflow_call" ]; then
  # prepare-release.yml reusable call. Caller event may be workflow_dispatch,
  # pull_request, or (rarely) workflow_call.
  TAG="${INPUT_TAG#refs/tags/}"
  MODE="git_ref"
elif [ "$EVENT_NAME" = "workflow_dispatch" ]; then
  # Direct emergency rebuild of release.yml.
  # Never check out from the dispatch input (CodeQL cache-poisoning).
  # Operator must run the workflow FROM the tag ref.
  if [ "$REF_TYPE" != "tag" ]; then
    echo "::error::workflow_dispatch must target the tag ref (gh workflow run --ref vX.Y.Z -f tag_name=vX.Y.Z). Got ref_type=$REF_TYPE ref_name=$REF_NAME"
    exit 1
  fi
  TAG="$REF_NAME"
  INPUT="${INPUT_TAG#refs/tags/}"
  if [ -n "$INPUT" ] && [ "$INPUT" != "$TAG" ]; then
    echo "::error::tag_name input ($INPUT) must match dispatch tag ref ($TAG)"
    exit 1
  fi
  MODE="workflow_sha"
elif [ "$EVENT_NAME" = "release" ]; then
  TAG="${RELEASE_TAG#refs/tags/}"
  MODE="git_ref"
else
  # Inherited caller events without from_prepare (should not happen once
  # prepare-release always passes the flag). Fall back to the input tag.
  TAG="${INPUT_TAG#refs/tags/}"
  MODE="git_ref"
fi

if [ -z "$TAG" ]; then
  echo "::error::No tag name provided"
  exit 1
fi
if ! [[ "$TAG" =~ ^v?[0-9]+\.[0-9]+\.[0-9]+([.-][A-Za-z0-9.-]+)?$ ]]; then
  echo "::error::Invalid tag format: $TAG"
  exit 1
fi

if [ -n "${GITHUB_OUTPUT:-}" ]; then
  echo "checkout_mode=$MODE" >> "$GITHUB_OUTPUT"
  echo "tag=$TAG" >> "$GITHUB_OUTPUT"
  echo "git_ref=refs/tags/$TAG" >> "$GITHUB_OUTPUT"
fi

echo "Resolved tag: $TAG (mode=$MODE)"
