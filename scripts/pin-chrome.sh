#!/usr/bin/env bash
#
# Regenerate the pinned Chrome for Testing table in
# crates/chatty-core/src/services/browser/provisioning.rs.
#
# Chrome for Testing publishes no checksums of its own, so we compute them here
# and commit them — the committed hash is the trust anchor, not the transport.
# Each archive is downloaded twice and the byte count is checked against
# Content-Length, so a silently truncated stream cannot produce a bad pin.
#
# Usage:  ./scripts/pin-chrome.sh [version]
#         (default: the current Chrome for Testing Stable channel)
#
# Then paste the printed table into provisioning.rs and bump PINNED_VERSION.

set -euo pipefail

FEED="https://googlechromelabs.github.io/chrome-for-testing/last-known-good-versions-with-downloads.json"
PLATFORMS=(linux64 mac-arm64 mac-x64 win64)

JSON="$(curl -sf "$FEED")"
VERSION="${1:-$(printf '%s' "$JSON" | python3 -c 'import sys,json;print(json.load(sys.stdin)["channels"]["Stable"]["version"])')}"

echo "# Chrome for Testing ${VERSION}" >&2
echo >&2

sha_of() {  # url -> sha256, verifying the byte count
  local url="$1"
  local expect got tmp sha
  expect="$(curl -sfI "$url" | awk 'tolower($1)=="content-length:"{print $2}' | tr -d '\r')"
  tmp="$(mktemp)"
  curl -sfL "$url" -o "$tmp"
  got="$(wc -c < "$tmp" | tr -d ' ')"
  if [[ "$expect" != "$got" ]]; then
    echo "TRUNCATED download for $url ($got of $expect bytes)" >&2
    rm -f "$tmp"
    return 1
  fi
  sha="$(shasum -a 256 "$tmp" | awk '{print $1}')"
  rm -f "$tmp"
  printf '%s' "$sha"
}

echo "PINNED_VERSION = \"${VERSION}\""
echo
for plat in "${PLATFORMS[@]}"; do
  url="https://storage.googleapis.com/chrome-for-testing-public/${VERSION}/${plat}/chrome-${plat}.zip"
  echo -n "  ${plat}: " >&2
  first="$(sha_of "$url")"
  second="$(sha_of "$url")"
  if [[ "$first" != "$second" ]]; then
    echo "UNSTABLE hash for $plat — got $first then $second" >&2
    exit 1
  fi
  echo "ok" >&2
  echo "  ${plat}"
  echo "    url:    ${url}"
  echo "    sha256: ${first}"
done
