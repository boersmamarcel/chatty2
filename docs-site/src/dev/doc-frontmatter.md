---
audience: [contributor, agent]
source_files:
  - scripts/check-docs-frontmatter.sh
related:
  - ./dev/guides/contribute-docs.md
  - ./dev/agents.md
---

# Doc frontmatter schema

**When to read this:** Add or edit YAML frontmatter on a markdown doc page.

Frontmatter is **optional**. Pages without it pass the lint. When a page
starts with a `---` fence, the block must match this schema:

```yaml
---
audience: [contributor, agent]
source_files:
  - crates/chatty-core/src/tools/mod.rs
related:
  - ./dev/reference/tools-catalog.md
---
```

| Field | Required if fence present | Value |
|-------|---------------------------|-------|
| `audience` | yes | List; each item one of `contributor`, `agent`, `user` |
| `source_files` | no | List of repo-relative paths the page describes |
| `related` | no | List of related doc links or issue ids |

Unknown keys fail the lint. Inline lists (`audience: [agent]`) and block
lists (`- agent`) are both accepted. Paths in `related` are written relative
to `docs-site/src/` (for example `./dev/guides/test.md`), whatever directory
the page itself lives in.

## Lint

```bash
make docs-check-frontmatter
# or
bash scripts/check-docs-frontmatter.sh
```

CI runs the same script in `.github/workflows/docs.yml`. The checker walks
`docs/`, `docs-site/src/`, `AGENTS.md`, and `CLAUDE.md`. Synced copies under
`docs-site/src/dev/architecture/` inherit whatever the source file in `docs/`
has. How the frontmatter fits into the rest of the docs workflow is in
[Contribute to the docs](./guides/contribute-docs.md).
