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

## Commands

```bash
make docs-gen   # docs/generated/*.md
make docs       # sync + mdbook build
make docs-serve # http://localhost:3000
```

Output: `docs-site/book/` (gitignored).
