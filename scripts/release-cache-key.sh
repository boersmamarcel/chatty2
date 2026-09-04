#!/usr/bin/env bash
# release-cache-key.sh — print the actions/cache key for the release-profile
# build cache.
#
# One writer, several readers: ci.yml's `warm-release-cache` job (push to
# main) saves under this key; release.yml's build jobs restore it (release
# runs are restore-only so they can never write default-branch caches).
#
# The hash covers Cargo.lock and the rustc version, but ignores the workspace
# crates' own `version = ...` lines: a release bump rewrites those for every
# member while no dependency artifact changes, so the cache warmed from the
# pre-bump main commit must still be an exact match on the release tag.
#
# Needs cargo, jq, awk and sha256sum (all present on GitHub-hosted runners).
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

members="$(cargo metadata --no-deps --format-version 1 | jq -r '.packages[].name')"
hash="$(
  {
    rustc -V
    awk -v members="$members" '
      BEGIN { n = split(members, m, "\n"); for (i = 1; i <= n; i++) ws[m[i]] = 1 }
      /^name = "/ { name = $0; sub(/^name = "/, "", name); sub(/"$/, "", name); in_ws = (name in ws) }
      in_ws && /^version = / { print "version = \"0.0.0\""; next }
      { print }
    ' Cargo.lock
  } | sha256sum | cut -c1-16
)"
echo "cargo-release-${RUNNER_OS:-$(uname -s)}-${hash}"
