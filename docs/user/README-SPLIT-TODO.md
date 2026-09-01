# README split — decisions (DOC-15 / AGE-97)

Marcel asked the agent to continue despite `owner:human` (2026-09-01): trim,
GIF placement, and tone pass as a reasonable split rather than stalling.

## Split

| Surface | Role |
|---------|------|
| `README.md` | ~70-line landing: what this repo is, marketing link, docs site, releases, `make` quick-start. Hero GIF only. |
| `docs-site/src/user/*` | User manual (how-to + explanation). Edit in place. |
| [boersmamarcel/chatty](https://github.com/boersmamarcel/chatty) | Marketing site. Extra demos can stay there. |
| `assets/animations/` | Source GIFs in this repo. |

## GIF placement

`docs-sync` copies **docs-sized** GIFs into `docs-site/src/assets/animations/`
so mdBook can inline them. Files larger than ~3 MB are **linked**, not copied
(GitHub Pages and browser cost).

| File | Size (approx) | Where |
|------|---------------|-------|
| `hero_high_quality.gif` | 11 MB | README only |
| `hero.gif` | 4 MB | unused duplicate; leave in `assets/` |
| `add_provider_and_model.gif` | 50 MB | link from getting-started |
| `mermaid.gif` | 0.5 MB | features.md |
| `codehighlighting.gif` | 0.2 MB | features.md |
| `advanced_math_rendering.gif` | 2.3 MB | features.md |
| `advanced_token_tracking.gif` | 2 MB | features.md |
| `webfetch.gif` | 1.7 MB | agentic-tools.md |
| `advanced_internet_access_settings.gif` | 1.2 MB | agentic-tools.md |
| `file_add_edit_delete.gif` | 19 MB | link from features / agentic-tools |
| `shell_command.gif` | 6 MB | link from agentic-tools |
| `mcp_add_edit_delete2.gif` | 18 MB | link from agentic-tools |

## Tone

`/user/` pages are docs voice (when to read, numbered steps, tables). Sales
lines (“no middleman”, “not another Electron wrapper” as a punch) were
dropped or restated as facts. Overview still explains *why* the product
exists; it is not a second marketing homepage.

## Follow-up for Marcel (optional)

- Re-encode the three huge walkthrough GIFs if they should play inline.
- Tone tweaks on `/user/` if anything still reads as README leftover.
