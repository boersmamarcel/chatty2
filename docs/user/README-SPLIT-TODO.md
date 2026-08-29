# README split — human review checklist (DOC-15 / AGE-97)

**Owner:** `owner:human` — Marcel reviews tone and which GIFs move vs stay.

## AI-completed (this migration)

User guide pages now live under `docs-site/src/user/`:

| Page | Source README section |
|------|----------------------|
| `getting-started.md` | Getting Started |
| `overview.md` | Why Chatty? |
| `agents.md` | Agents |
| `agentic-tools.md` | Tools & MCP (summary) |
| `memory-and-skills.md` | Agent Memory & Skills |
| `sub-agents.md` | Sub-Agent Orchestration |
| `security.md` | Security & Sandboxing |
| `features.md` | Features |
| `terminal.md` | chatty-tui — Terminal Interface |
| `providers-and-models.md` | Features → Multi-Provider (existing) |

## Remaining for human

1. **Trim `README.md`** to ~80 lines: what Chatty is, marketing link, docs site link, releases, minimal dev quick-start.
2. **GIF placement** — decide which `assets/animations/*.gif` stay in README vs move to user docs or marketing repo.
3. **Tone pass** on migrated `/user/` pages (marketing voice vs docs voice).
4. **Remove duplicated content** from README once links to docs site are in place.

Published site: GitHub Pages mdBook (`make docs-serve` locally).
