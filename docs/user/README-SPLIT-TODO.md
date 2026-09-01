# README split — decisions (DOC-15 / AGE-97)

`owner:human` was lifted for this pass (Marcel: “make a reasonable split”).
The issue stays open for review; it is not closed by this change.

## Done

1. **`README.md` trimmed** to a landing page: what Chatty is, marketing link,
   docs site, releases, user-guide links, minimal `make` / `cargo run`
   quick-start.
2. **GIF placement** (source of truth remains `assets/animations/` in this
   repo — there was no Linear attachment). `docs-sync` copies them to
   `docs-site/src/user/img/` (gitignored) so the mdBook can render them.
3. **Tone pass** on `/user/` pages: instructional voice, “when to read”
   dropped in favor of a one-line purpose sentence, DOC-15 “pending review”
   notes removed, developer internals (`ModelConfig`, `ProviderType`) kept
   out of user pages.
4. **Duplicated README sections** removed; those topics live only under
   `/user/` and `/dev/reference/`.

## GIF map

| Asset | Size (approx.) | Where it renders |
|-------|----------------|------------------|
| `hero_high_quality.gif` | 11 MB | README hero + [overview](../../docs-site/src/user/overview.md) |
| `hero.gif` | 4 MB | Unused (smaller duplicate). Stays in `assets/animations/` |
| `add_provider_and_model.gif` | 50 MB | [getting-started](../../docs-site/src/user/getting-started.md) |
| `advanced_math_rendering.gif` | 2 MB | [features](../../docs-site/src/user/features.md) |
| `mermaid.gif` | 0.5 MB | features |
| `codehighlighting.gif` | 0.2 MB | features |
| `advanced_token_tracking.gif` | 2 MB | features |
| `file_add_edit_delete.gif` | 20 MB | [agentic-tools](../../docs-site/src/user/agentic-tools.md) |
| `shell_command.gif` | 6 MB | agentic-tools |
| `webfetch.gif` | 2 MB | agentic-tools |
| `mcp_add_edit_delete2.gif` | 18 MB | agentic-tools |
| `advanced_internet_access_settings.gif` | 1 MB | agentic-tools |

Nothing was moved to [boersmamarcel/chatty](https://github.com/boersmamarcel/chatty).
Marketing can keep its own copies; this repo still owns the files.

## Still optional (human)

- Re-encode the 18–50 MB GIFs if GitHub Pages or the README feels too heavy.
- Swap `hero_high_quality.gif` for `hero.gif` on the README if load time
  matters more than fidelity.
- A further tone pass if any page still reads too marketing-heavy or too
  developer-heavy.
