# Contribute to the docs

**When to read this:** You are adding or changing a page on this site and want to know which file to edit, how to preview it, and what CI checks before it merges.

## Goal

A change to the right source file, previewed locally, passing `make docs-check`, landing in the same PR as the code it describes.

## Prerequisites

- [mdBook](https://rust-lang.github.io/mdBook/) (`cargo install mdbook`) for `make docs` / `make docs-serve`.
- [lychee](https://github.com/lycheeverse/lychee) (`cargo install lychee --locked`) and `python3` for `make docs-check-links` and the other check scripts.

## Steps

### 1. Find the file to edit

Most of the site is synced from the repository by `make docs-sync`. Editing the copy under `docs-site/src/dev/` is lost on the next sync.

| Built page | Edit instead |
|------------|--------------|
| `src/dev/architecture/*.md` | `docs/*.md` |
| `src/dev/adrs/*.md` | `docs/research/*.md` |
| `src/dev/research/modules/*.md` | `docs/research/modules/*.md` |
| `src/dev/agents.md` | `AGENTS.md` |
| `src/dev/reference/*.md` | The tables in `scripts/gen-docs-reference.sh`, then `make docs-gen` |
| `src/dev/crates/*.md` | `crates/*/README.md` |

Hand-written pages, edited in place under `docs-site/src/`: `index.md`, `user/*`, `dev/start/*`, `dev/guides/*`, `dev/where-to-look.md`, `dev/crates.md`, `dev/contributing-patterns.md`, `dev/glossary.md`, `dev/ci-reference.md`, `dev/doc-frontmatter.md`. `CLAUDE.md` and `docs/INDEX.md` are not synced.

### 2. Pick the page type

- **Tutorial** (`dev/start/`): learn by building; ends with something running.
- **How-to** (`dev/guides/`): one task, done. Use the template below.
- **Explanation** (`docs/*.md`, synced to `dev/architecture/`): why it is built this way.
- **Reference** (`dev/reference/`, `dev/crates/`, `dev/ci-reference.md`, `dev/glossary.md`): lookup tables.

Working notes — point-in-time plans, audits, bug lists — go to `docs/archive/` (not synced, not in `docs/INDEX.md`) or to Linear, never under `docs/`.

### 3. Write to the template

Every page starts with the title and a one-line reader test:

```markdown
# Add a widget

**When to read this:** You need a new panel in the desktop app.
```

How-to pages then follow **goal · prerequisites · steps · verify · checklist · common mistakes**, skipping a section only when it would be empty. Rules that keep the site coherent:

- Links inside `docs-site/src` are relative to the page's own directory (`../architecture/stream-manager.md` from a guide). Link Rust source files with full GitHub URLs (`https://github.com/boersmamarcel/chatty2/blob/main/...`), never with relative paths, since the page moves when it is published.
- User guides (`src/user/*`) must not carry contributor material: no ticket ids, crate paths, `.rs` file names or GPUI internals. `make docs-check-leakage` fails otherwise.
- Do not add docs.rs badges: no workspace crate is published, and the links 404.
- Prefer describing the behaviour over citing the ticket that introduced it.

Callouts use GitHub's alert syntax and render as boxes on the site:

```markdown
> [!NOTE]
> Background the reader can skip.

> [!TIP]
> A shortcut.

> [!WARNING]
> Something that costs time or data if missed.
```

### 4. Optional frontmatter

A page may begin with a YAML block declaring `audience`, `source_files` and `related`. The schema and the lint are in [Doc frontmatter schema](../doc-frontmatter.md).

### 5. Register a new page

A new `docs/*.md` needs a row in `docs/INDEX.md` and an entry in `docs-site/src/SUMMARY.md`; a new hand-written page needs the `SUMMARY.md` entry. `make docs-check-nav` fails when either is missing. A new tool, event, setting or env var needs a row in `scripts/gen-docs-reference.sh`; `make docs-check-reference` diffs those tables against the source.

### 6. Preview and check

```bash
make docs-serve        # docs-gen + docs-sync + mdbook serve, opens http://localhost:3000
make docs-check        # everything below
```

| Target | Checks |
|--------|--------|
| `make docs-check-links` | Builds the site, then link-checks the sources (lychee) and the relative links on the *built* site, where `docs-sync` has moved pages into new directories |
| `make docs-check-nav` | `docs/INDEX.md` and `SUMMARY.md` list every page |
| `make docs-check-frontmatter` | Frontmatter matches the schema |
| `make docs-check-leakage` | User guides carry no contributor material |
| `make docs-check-reference` | Reference tables match `tool_registry.rs`, `StreamManagerEvent`, every `EventEmitter` enum, and the settings structs |

CI runs the same scripts from `.github/workflows/docs.yml`. Docs-only PRs do not compile the workspace.

### 7. Ship it in the same PR

**Same-PR rule.** If a change alters a fact a page claims, update that page in the same PR. A page is stale when someone following it would do the wrong thing, look in the wrong place, or miss a step; internal refactors that keep documented facts true, formatting, comments and dependency bumps need no doc change. The follow-up workflows (`update-agent-docs.yml`, `update-readme.yml`) are a safety net, not a reason to skip this. Found a stale page you are not fixing? File a [Doc drift](https://github.com/boersmamarcel/chatty2/issues/new?template=doc-drift.yml) issue. The full policy is in [CONTRIBUTING.md](https://github.com/boersmamarcel/chatty2/blob/main/CONTRIBUTING.md).

## Verify

`make docs-check` exits 0 and the page reads correctly at `http://localhost:3000`, in both light and dark theme.

## Checklist

- [ ] Edited the source file, not the synced copy
- [ ] `# Title` and `**When to read this:**` line present
- [ ] How-to sections in template order
- [ ] Relative links within the site; GitHub URLs for source files
- [ ] New page listed in `SUMMARY.md` (and `docs/INDEX.md` for `docs/*.md`)
- [ ] `make docs-check` green
- [ ] Docs change is in the PR that changed the behaviour

## Common mistakes

| Mistake | Do this instead |
|---------|-----------------|
| Editing `docs-site/src/dev/architecture/foo.md` | Edit `docs/foo.md`; the sync overwrites the copy |
| Editing a `reference/*.md` table by hand | Edit `scripts/gen-docs-reference.sh`, run `make docs-gen` |
| A link that works in the repo but 404s on the site | Run `make docs-check-links`; it checks the built site |
| A plan or audit added under `docs/` | `docs/archive/` or Linear |
| Ticket ids or crate paths in `src/user/` | Move the detail to a dev page; `docs-check-leakage` blocks it |
| New page without a `SUMMARY.md` entry | mdBook does not render it; `docs-check-nav` fails |
