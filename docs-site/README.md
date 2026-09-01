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
| `src/dev/contributing-patterns.md` | `CLAUDE.md` |
| `src/dev/reference/*.md` | Regenerate via `make docs-gen` |
| `src/dev/crates/*.md` | `crates/*/README.md` |

Hand-written pages (edit in place): `src/index.md`, `src/user/*`, `src/dev/guides/*`, `src/dev/where-to-look.md`, `src/dev/crates.md`.

Optional YAML frontmatter (`audience`, `source_files`, `related`) is documented in `src/dev/guides/doc-frontmatter.md` and linted by `make docs-check-frontmatter`.

`make docs-sync` also copies docs-sized GIFs from `assets/animations/` into
`src/assets/animations/` (see `docs/user/README-SPLIT-TODO.md`). Do not commit
those copies; they are build artifacts like other synced pages.

## Commands

```bash
make docs-gen   # docs/generated/*.md
make docs       # sync + mdbook build
make docs-serve # http://localhost:3000
```

Output: `docs-site/book/` (gitignored).
