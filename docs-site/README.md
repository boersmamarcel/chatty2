# docs-site

mdBook developer documentation for Chatty. **Do not edit synced pages here** —
edit the source files and rebuild.

## Source of truth

| Built page | Edit instead |
|------------|--------------|
| `src/dev/architecture/*.md` | `docs/*.md` |
| `src/dev/adrs/*.md` | `docs/research/*.md` |
| `src/dev/research/modules/*.md` | `docs/research/modules/*.md` |
| `src/dev/agents.md` | `AGENTS.md` |
| `src/dev/reference/*.md` | The tables in `scripts/gen-docs-reference.sh`, then `make docs-gen` |
| `src/dev/crates/*.md` | `crates/*/README.md` |

`CLAUDE.md` and `docs/INDEX.md` are not synced to the site.

Hand-written pages (edit in place): `src/index.md`, `src/user/*`,
`src/dev/start/*`, `src/dev/guides/*`, `src/dev/where-to-look.md`,
`src/dev/crates.md`, `src/dev/contributing-patterns.md`,
`src/dev/glossary.md`, `src/dev/ci-reference.md`,
`src/dev/doc-frontmatter.md`.

Optional YAML frontmatter (`audience`, `source_files`, `related`) is documented in `src/dev/doc-frontmatter.md` and linted by `make docs-check-frontmatter`. The full editing workflow is in `src/dev/guides/contribute-docs.md`.

`make docs-sync` also copies docs-sized GIFs from `assets/animations/` into
`src/assets/animations/`. Files larger than ~3 MB are linked instead of
copied (GitHub Pages and browser cost). Do not commit those copies; they are
build artifacts like other synced pages. To re-record the GIFs themselves, see
[`scripts/animations/README.md`](../scripts/animations/README.md).

## Commands

```bash
make docs-gen    # docs/generated/*.md
make docs        # gen + sync + mdbook build
make docs-serve  # http://localhost:3000
make docs-check  # links, nav, frontmatter, user-guide leakage, reference drift
```

Output: `docs-site/book/` (gitignored).
