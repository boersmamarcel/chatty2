#!/usr/bin/env bash
# AGE-251: verify relative links on the *built* site, not just the sources.
# docs-sync.sh moves pages into new directories, so a link that is valid in
# the repo can be dead once published. Run after `mdbook build docs-site`.
set -euo pipefail
cd "$(dirname "$0")/.."

BOOK="docs-site/book"
if [[ ! -f "$BOOK/index.html" ]]; then
  echo "missing $BOOK/index.html — run 'mdbook build docs-site' first"
  exit 1
fi

python3 - "$BOOK" <<'PY'
import os
import re
import sys
from urllib.parse import unquote

book = sys.argv[1]
href_re = re.compile(r'(?:href|src)="([^"#]+)(?:#[^"]*)?"')
dead = []
for root, _dirs, files in os.walk(book):
    for name in files:
        if not name.endswith(".html") or name in ("print.html", "404.html", "toc.html"):
            continue
        page = os.path.join(root, name)
        with open(page, encoding="utf-8") as fh:
            html = fh.read()
        for href in href_re.findall(html):
            if re.match(r"^[a-z][a-z0-9+.-]*:", href) or href.startswith("/") or href.startswith("data:"):
                continue
            target = os.path.normpath(os.path.join(os.path.dirname(page), unquote(href)))
            if os.path.exists(target) or os.path.exists(target + ".html"):
                continue
            dead.append((os.path.relpath(page, book), href))

if dead:
    for page, href in dead:
        print(f"dead link: {page} -> {href}")
    print(f"{len(dead)} dead relative link(s) on the built site")
    sys.exit(1)
print("site link check: OK")
PY
