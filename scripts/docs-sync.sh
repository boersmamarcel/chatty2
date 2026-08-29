#!/usr/bin/env bash
# Sync in-repo markdown into docs-site/src before mdbook build.
# Single source of truth: edit docs/, AGENTS.md, CLAUDE.md in repo root — not copies in docs-site/.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SITE_SRC="$ROOT/docs-site/src"
ARCH="$SITE_SRC/dev/architecture"
ADRS="$SITE_SRC/dev/adrs"
REF="$SITE_SRC/dev/reference"
USER="$SITE_SRC/user"

mkdir -p "$ARCH" "$ADRS" "$REF" "$USER"

copy() {
  local src="$1" dest="$2"
  install -D -m 644 "$src" "$dest"
}

# Root agent/contributor docs
copy "$ROOT/AGENTS.md" "$SITE_SRC/dev/agents.md"
copy "$ROOT/CLAUDE.md" "$SITE_SRC/dev/contributing-patterns.md"
copy "$ROOT/docs/INDEX.md" "$SITE_SRC/dev/doc-index.md"

# Architecture & design (exclude research subdir and generated)
for f in "$ROOT"/docs/*.md; do
  base="$(basename "$f")"
  [[ "$base" == "INDEX.md" ]] && continue
  copy "$f" "$ARCH/$base"
done

# ADRs / research decisions
if compgen -G "$ROOT/docs/research/*.md" > /dev/null; then
  for f in "$ROOT"/docs/research/*.md; do
    copy "$f" "$ADRS/$(basename "$f")"
  done
fi

# Per-crate READMEs → dev/crates/
CRATES_DIR="$SITE_SRC/dev/crates"
mkdir -p "$CRATES_DIR"
for readme in "$ROOT"/crates/*/README.md; do
  crate="$(basename "$(dirname "$readme")")"
  copy "$readme" "$CRATES_DIR/$crate.md"
done

# Generated reference pages (from gen-docs-reference.sh)
if [[ -d "$ROOT/docs/generated" ]]; then
  for f in "$ROOT"/docs/generated/*.md; do
    [[ -e "$f" ]] || continue
    copy "$f" "$REF/$(basename "$f")"
  done
fi

echo "docs-sync: synced markdown into $SITE_SRC"
