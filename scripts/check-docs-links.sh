#!/usr/bin/env bash
# AGE-117: verify markdown links in source docs (not synced mdBook copies).
# Requires: lychee (https://github.com/lycheeverse/lychee)
set -euo pipefail
cd "$(dirname "$0")/.."

if ! command -v lychee >/dev/null 2>&1; then
  echo "lychee not found; install with: cargo install lychee --locked"
  exit 1
fi

bash scripts/gen-docs-reference.sh
bash scripts/docs-sync.sh

# Source-of-truth paths only. docs-site/src copies rewrite relative paths and
# would duplicate checks; mdBook HTML pulls in RESERVED via cross-links.
lychee \
  --config .lychee.toml \
  --offline \
  --no-progress \
  'docs/**/*.md' \
  'AGENTS.md' \
  'CLAUDE.md' \
  'CONTRIBUTING.md' \
  'crates/*/README.md' \
  'docs-site/src/index.md' \
  'docs-site/src/user/**/*.md' \
  'docs-site/src/dev/guides/**/*.md' \
  'docs-site/src/dev/where-to-look.md'

echo "docs link check: OK"
