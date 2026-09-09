#!/usr/bin/env bash
# Sync in-repo markdown into docs-site/src before mdbook build.
# Single source of truth: edit docs/, AGENTS.md and crates/*/README.md in the
# repo — not the copies under docs-site/src (they are gitignored).
#
# Pages move directory when they are copied, so their relative links are
# rewritten per destination (AGE-251). Links to files that are not on the site
# (Rust sources, workflows, RESERVED.md, …) become GitHub URLs.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SITE_SRC="$ROOT/docs-site/src"
ARCH="$SITE_SRC/dev/architecture"
ADRS="$SITE_SRC/dev/adrs"
REF="$SITE_SRC/dev/reference"
MODULES="$SITE_SRC/dev/research/modules"
CRATES_DIR="$SITE_SRC/dev/crates"
GH="https://github.com/boersmamarcel/chatty2/blob/main"

# Start clean so a page deleted from the repo cannot linger as a stale copy.
rm -rf "$ARCH" "$ADRS" "$REF" "$MODULES" "$CRATES_DIR" "$SITE_SRC/dev/agents.md" "$SITE_SRC/assets"
mkdir -p "$ARCH" "$ADRS" "$REF" "$MODULES" "$CRATES_DIR" "$SITE_SRC/user"

copy() {
  local src="$1" dest="$2"
  install -D -m 644 "$src" "$dest"
}

# rewrite <file> <sed-expression>...  — apply link rewrites in place.
rewrite() {
  local file="$1"; shift
  local args=()
  for expr in "$@"; do args+=(-e "$expr"); done
  sed -i "${args[@]}" "$file"
}

# Rewrites shared by every page that came from docs/ (one level below root).
# Order matters: the specific, on-site targets first; the catch-all to GitHub last.
docs_common=(
  's|](\./research/modules/|](../research/modules/|g'
  's|](research/modules/|](../research/modules/|g'
  's|](\./research/README\.md|](../adrs/README.md|g'
  's|](research/README\.md|](../adrs/README.md|g'
  's|](\./research/|](../adrs/|g'
  's|](research/|](../adrs/|g'
  's|](\.\./docs-site/src/dev/guides/|](../guides/|g'
  's|](\.\./docs-site/src/user/|](../../user/|g'
  's|](\.\./docs-site/src/dev/|](../|g'
  's|](\.\./AGENTS\.md|](../agents.md|g'
  's|](\.\./CONTRIBUTING\.md|](https://github.com/boersmamarcel/chatty2/blob/main/CONTRIBUTING.md|g'
  's|](\.\./CLAUDE\.md|](https://github.com/boersmamarcel/chatty2/blob/main/CLAUDE.md|g'
  's|](\.\./README\.md|](https://github.com/boersmamarcel/chatty2/blob/main/README.md|g'
  's|](\.\./RESERVED\.md|](https://github.com/boersmamarcel/chatty2/blob/main/RESERVED.md|g'
  's|](\.\./crates/\([a-z0-9-]*\)/README\.md|](../crates/\1.md|g'
  's|](\.\./crates/|](https://github.com/boersmamarcel/chatty2/tree/main/crates/|g'
  's|](\.\./\.github/|](https://github.com/boersmamarcel/chatty2/blob/main/.github/|g'
  's|](\.\./scripts/|](https://github.com/boersmamarcel/chatty2/blob/main/scripts/|g'
  's|](\.\./modules/|](https://github.com/boersmamarcel/chatty2/tree/main/modules/|g'
  's|](\.\./wit/|](https://github.com/boersmamarcel/chatty2/tree/main/wit/|g'
  's|](\.\./archive/|](https://github.com/boersmamarcel/chatty2/blob/main/docs/archive/|g'
  's|](archive/|](https://github.com/boersmamarcel/chatty2/blob/main/docs/archive/|g'
  's|](\./archive/|](https://github.com/boersmamarcel/chatty2/blob/main/docs/archive/|g'
)

# Root agent guide → dev/agents.md (links were relative to the repo root)
copy "$ROOT/AGENTS.md" "$SITE_SRC/dev/agents.md"
rewrite "$SITE_SRC/dev/agents.md" \
  's|](docs-site/src/user/|](../user/|g' \
  's|](docs-site/src/dev/|](./|g' \
  's|](docs/INDEX\.md|](https://github.com/boersmamarcel/chatty2/blob/main/docs/INDEX.md|g' \
  's|](docs/research/modules/|](./research/modules/|g' \
  's|](docs/research/|](./adrs/|g' \
  's|](docs/archive/|](https://github.com/boersmamarcel/chatty2/blob/main/docs/archive/|g' \
  's|](docs/\([A-Za-z0-9_-]*\)\.md|](./architecture/\1.md|g' \
  's|](crates/\([a-z0-9-]*\)/README\.md|](./crates/\1.md|g' \
  's|](CONTRIBUTING\.md|](https://github.com/boersmamarcel/chatty2/blob/main/CONTRIBUTING.md|g' \
  's|](CLAUDE\.md|](https://github.com/boersmamarcel/chatty2/blob/main/CLAUDE.md|g' \
  's|](README\.md|](https://github.com/boersmamarcel/chatty2/blob/main/README.md|g' \
  's|](RESERVED\.md|](https://github.com/boersmamarcel/chatty2/blob/main/RESERVED.md|g' \
  's|](\.github/|](https://github.com/boersmamarcel/chatty2/blob/main/.github/|g' \
  's|](scripts/|](https://github.com/boersmamarcel/chatty2/blob/main/scripts/|g' \
  's|](crates/|](https://github.com/boersmamarcel/chatty2/tree/main/crates/|g' \
  's|](modules/|](https://github.com/boersmamarcel/chatty2/tree/main/modules/|g' \
  's|](Makefile|](https://github.com/boersmamarcel/chatty2/blob/main/Makefile|g'

# Architecture & explanation pages: docs/*.md → dev/architecture/
# (docs/archive/ and docs/generated/ are deliberately not copied.)
for f in "$ROOT"/docs/*.md; do
  base="$(basename "$f")"
  [[ "$base" == "INDEX.md" ]] && continue
  copy "$f" "$ARCH/$base"
  rewrite "$ARCH/$base" "${docs_common[@]}"
done

# ADRs / research decisions: docs/research/*.md → dev/adrs/
for f in "$ROOT"/docs/research/*.md; do
  base="$(basename "$f")"
  copy "$f" "$ADRS/$base"
  rewrite "$ADRS/$base" \
    's|](\./modules/|](../research/modules/|g' \
    's|](modules/|](../research/modules/|g' \
    's|](\.\./\([A-Za-z0-9_-]*\)\.md|](../architecture/\1.md|g' \
    's|](\.\./docs-site/src/dev/guides/|](../guides/|g' \
    's|](\.\./\.\./docs-site/src/dev/guides/|](../guides/|g' \
    's|](\.\./\.\./docs-site/src/user/|](../../user/|g' \
    's|](\.\./\.\./AGENTS\.md|](../agents.md|g' \
    's|](\.\./\.\./crates/\([a-z0-9-]*\)/README\.md|](../crates/\1.md|g' \
    's|](\.\./\.\./crates/\([a-z0-9-]*\)/\?)|](../crates/\1.md)|g' \
    's|](\.\./\.\./crates/|](https://github.com/boersmamarcel/chatty2/tree/main/crates/|g' \
    's|](\.\./\.\./RESERVED\.md|](https://github.com/boersmamarcel/chatty2/blob/main/RESERVED.md|g' \
    's|](\.\./\.\./CLAUDE\.md|](https://github.com/boersmamarcel/chatty2/blob/main/CLAUDE.md|g' \
    's|](\.\./\.\./CONTRIBUTING\.md|](https://github.com/boersmamarcel/chatty2/blob/main/CONTRIBUTING.md|g' \
    's|](\.\./\.\./\.github/|](https://github.com/boersmamarcel/chatty2/blob/main/.github/|g' \
    's|](\.\./\.\./scripts/|](https://github.com/boersmamarcel/chatty2/blob/main/scripts/|g' \
    's|](\.\./\.\./modules/|](https://github.com/boersmamarcel/chatty2/tree/main/modules/|g' \
    's|](\.\./\.\./\.\./harbor-chatty|](https://github.com/boersmamarcel/harbor-chatty|g' \
    's|](\.\./archive/|](https://github.com/boersmamarcel/chatty2/blob/main/docs/archive/|g'
done

# Research module pages: docs/research/modules/*.md → dev/research/modules/
for f in "$ROOT"/docs/research/modules/*.md; do
  base="$(basename "$f")"
  copy "$f" "$MODULES/$base"
  rewrite "$MODULES/$base" \
    's|](\.\./\([A-Za-z0-9_-]*\)\.md|](../../adrs/\1.md|g' \
    's|](\.\./\.\./\([A-Za-z0-9_-]*\)\.md|](../../architecture/\1.md|g' \
    's|](\.\./\.\./\.\./docs-site/src/dev/guides/|](../../guides/|g' \
    's|](\.\./\.\./\.\./AGENTS\.md|](../../agents.md|g' \
    's|](\.\./\.\./\.\./crates/\([a-z0-9-]*\)/README\.md|](../../crates/\1.md|g' \
    's|](\.\./\.\./\.\./crates/\([a-z0-9-]*\)/\?)|](../../crates/\1.md)|g' \
    's|](\.\./\.\./\.\./crates/|](https://github.com/boersmamarcel/chatty2/tree/main/crates/|g' \
    's|](\.\./\.\./\.\./RESERVED\.md|](https://github.com/boersmamarcel/chatty2/blob/main/RESERVED.md|g' \
    's|](\.\./\.\./\.\./CLAUDE\.md|](https://github.com/boersmamarcel/chatty2/blob/main/CLAUDE.md|g' \
    's|](\.\./\.\./\.\./\.github/|](https://github.com/boersmamarcel/chatty2/blob/main/.github/|g' \
    's|](\.\./\.\./\.\./scripts/|](https://github.com/boersmamarcel/chatty2/blob/main/scripts/|g' \
    's|](\.\./\.\./\.\./modules/|](https://github.com/boersmamarcel/chatty2/tree/main/modules/|g' \
    's|](\.\./\.\./\.\./\.\./harbor-chatty|](https://github.com/boersmamarcel/harbor-chatty|g' \
    's|](\.\./\.\./archive/|](https://github.com/boersmamarcel/chatty2/blob/main/docs/archive/|g'
done

# Per-crate READMEs → dev/crates/<crate>.md. Source links point at GitHub.
for readme in "$ROOT"/crates/*/README.md; do
  crate="$(basename "$(dirname "$readme")")"
  copy "$readme" "$CRATES_DIR/$crate.md"
  rewrite "$CRATES_DIR/$crate.md" \
    "s|](\.\./\([a-z0-9-]*\)/README\.md|](./\1.md|g" \
    "s|](\.\./\([a-z0-9-]*\)/\?)|](./\1.md)|g" \
    "s|](\.\./\.\./docs/research/modules/|](../research/modules/|g" \
    "s|](\.\./\.\./docs/research/|](../adrs/|g" \
    "s|](\.\./\.\./docs/archive/|](https://github.com/boersmamarcel/chatty2/blob/main/docs/archive/|g" \
    "s|](\.\./\.\./docs/\([A-Za-z0-9_-]*\)\.md|](../architecture/\1.md|g" \
    "s|](\.\./\.\./docs-site/src/dev/guides/|](../guides/|g" \
    "s|](\.\./\.\./AGENTS\.md|](../agents.md|g" \
    "s|](\.\./\.\./RESERVED\.md|](https://github.com/boersmamarcel/chatty2/blob/main/RESERVED.md|g" \
    "s|](\.\./\.\./CLAUDE\.md|](https://github.com/boersmamarcel/chatty2/blob/main/CLAUDE.md|g" \
    "s|](\.\./\.\./modules/|](https://github.com/boersmamarcel/chatty2/tree/main/modules/|g" \
    "s|](\.\./\.\./wit/|](https://github.com/boersmamarcel/chatty2/tree/main/wit/|g" \
    "s|](\.\./\.\./\([a-z]*\)/|](https://github.com/boersmamarcel/chatty2/tree/main/\1/|g" \
    "s|](\.\./\([a-z0-9-]*\)/src/|](https://github.com/boersmamarcel/chatty2/tree/main/crates/\1/src/|g" \
    "s|](src/|](https://github.com/boersmamarcel/chatty2/tree/main/crates/$crate/src/|g" \
    "s|^\(\[[^]]*\]\): src/|\1: https://github.com/boersmamarcel/chatty2/tree/main/crates/$crate/src/|" \
    "s|^\(\[[^]]*\]\): \./src/|\1: https://github.com/boersmamarcel/chatty2/tree/main/crates/$crate/src/|" \
    "s|^\(\[[^]]*\]\): tests/|\1: https://github.com/boersmamarcel/chatty2/tree/main/crates/$crate/tests/|" \
    "s|^\(\[[^]]*\]\): \.\./\([a-z0-9-]*\)/README\.md|\1: ./\2.md|" \
    "s|](\./src/|](https://github.com/boersmamarcel/chatty2/tree/main/crates/$crate/src/|g" \
    "s|](tests/|](https://github.com/boersmamarcel/chatty2/tree/main/crates/$crate/tests/|g" \
    "s|](examples/|](https://github.com/boersmamarcel/chatty2/tree/main/crates/$crate/examples/|g" \
    "s|](Cargo\.toml|](https://github.com/boersmamarcel/chatty2/blob/main/crates/$crate/Cargo.toml|g"
done

# Generated reference pages (from gen-docs-reference.sh)
if [[ -d "$ROOT/docs/generated" ]]; then
  for f in "$ROOT"/docs/generated/*.md; do
    [[ -e "$f" ]] || continue
    copy "$f" "$REF/$(basename "$f")"
  done
fi

# Docs-sized GIFs only. The 15–50 MB walkthroughs stay in assets/animations/
# and are linked from GitHub, not copied into the book.
ANIM_SRC="$ROOT/assets/animations"
ANIM_DEST="$SITE_SRC/assets/animations"
mkdir -p "$ANIM_DEST"
for name in \
  hero.gif \
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

# Static screenshots (PNG) for the user guides, when present.
if [[ -d "$ROOT/assets/screenshots" ]]; then
  mkdir -p "$SITE_SRC/assets/screenshots"
  for f in "$ROOT"/assets/screenshots/*.png; do
    [[ -e "$f" ]] || continue
    copy "$f" "$SITE_SRC/assets/screenshots/$(basename "$f")"
  done
fi

# App icon for the site header.
if [[ -f "$ROOT/assets/app_icon/ai-2.png" ]]; then
  copy "$ROOT/assets/app_icon/ai-2.png" "$SITE_SRC/assets/logo.png"
fi

echo "docs-sync: synced markdown into $SITE_SRC"
