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

MODULES="$SITE_SRC/dev/research/modules"
mkdir -p "$ARCH" "$ADRS" "$REF" "$USER" "$MODULES"

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
  # mdBook layout: guides live at dev/guides/, not docs-site/src/dev/guides/
  sed -i 's|](../docs-site/src/dev/guides/|](../guides/|g' "$ARCH/$base"
done

# ADRs / research decisions
if compgen -G "$ROOT/docs/research/*.md" > /dev/null; then
  for f in "$ROOT"/docs/research/*.md; do
    copy "$f" "$ADRS/$(basename "$f")"
  done
fi

# Research module pages (M0–M4)
if compgen -G "$ROOT/docs/research/modules/*.md" > /dev/null; then
  for f in "$ROOT"/docs/research/modules/*.md; do
    copy "$f" "$MODULES/$(basename "$f")"
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

# Docs-sized GIFs only. Hero HQ + 15–50 MB walkthroughs stay in assets/animations/
# and are linked from user pages, not copied into the book.
ANIM_SRC="$ROOT/assets/animations"
ANIM_DEST="$SITE_SRC/assets/animations"
mkdir -p "$ANIM_DEST"
for name in \
  mermaid.gif \
  codehighlighting.gif \
  advanced_math_rendering.gif \
  advanced_token_tracking.gif \
  webfetch.gif \
  advanced_internet_access_settings.gif \
  artifact_pdf.gif \
  artifact_chart.gif \
  artifact_table.gif \
  artifact_markdown.gif \
  pr_status_bar.gif
do
  if [[ -f "$ANIM_SRC/$name" ]]; then
    copy "$ANIM_SRC/$name" "$ANIM_DEST/$name"
  fi
done

echo "docs-sync: synced markdown into $SITE_SRC"
