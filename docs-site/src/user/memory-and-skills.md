# Memory & skills

**When to read this:** Use persistent memory and reusable agent procedures across conversations.

## How memory works

Memory is stored locally in a binary file ([memvid-core](https://crates.io/crates/memvid-core)). By default, retrieval uses full-text search; enable **Semantic Search** in Settings for vector similarity (requires an embedding provider).

The agent calls `search_memory` when past context would help — facts, preferences, prior decisions, project conventions.

| Tool | What it does |
|------|--------------|
| `remember` | Store a fact, note, or preference with optional title and tags |
| `search_memory` | Retrieve memories via natural-language query |

### Storage paths

| Platform | Path |
|----------|------|
| macOS | `~/Library/Application Support/chatty/memory.mv2` |
| Linux | `~/.local/share/chatty/memory.mv2` |
| Windows | `%APPDATA%\chatty\memory.mv2` |

Memory is on by default. Toggle in **Settings → Code Execution**.

### Settings

- **Memory Browser** — browse, search, delete entries in **Settings → Memory**
- **Purge All Memory** — permanently delete all stored memories
- **Semantic Search** — vector similarity when an embedding provider is configured

## Skills — reusable procedures

Skills are named, multi-step procedures for recurring tasks:

- `save_skill` — store a procedure (e.g. `"deploy-to-staging"`, `"write-unit-tests"`)
- `search_memory` + `read_skill` — discover and load skill instructions before executing
- Skills appear in the `/` slash-command picker
- Workspace skills can live in `.claude/skills/` alongside your code

## Related

- [Agentic tools](./agentic-tools.md) — full tools catalog
- [Agents](./agents.md) — agent loop overview
