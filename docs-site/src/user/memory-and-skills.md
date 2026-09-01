# Memory & skills

**When to read this:** Persistent memory and reusable procedures across chats.

## How memory works

Memory is a local binary file ([memvid-core](https://crates.io/crates/memvid-core)).
Default retrieval is full-text search. **Semantic Search** in Settings adds
vector similarity (needs an embedding provider).

The agent calls `search_memory` when past context would help — facts,
preferences, prior decisions, project conventions.

| Tool | What it does |
|------|--------------|
| `remember` | Store a fact, note, or preference (optional title and tags) |
| `search_memory` | Retrieve memories by natural-language query |

### Storage paths

| Platform | Path |
|----------|------|
| macOS | `~/Library/Application Support/chatty/memory.mv2` |
| Linux | `~/.local/share/chatty/memory.mv2` |
| Windows | `%APPDATA%\chatty\memory.mv2` |

Memory is on by default. Toggle it in **Settings → Code Execution**.

### Settings

- **Memory Browser** — browse, search, inspect, or delete entries in **Settings → Memory**
- **Purge All Memory** — delete every stored memory
- **Semantic Search** — vector similarity when an embedding provider is configured

## Skills — reusable procedures

Skills are named multi-step procedures for recurring tasks:

- `save_skill` stores a procedure under a name (`"deploy-to-staging"`, `"write-unit-tests"`)
- The agent uses `search_memory` to find relevant skills, then `read_skill` to load instructions
- Skills appear in the `/` slash-command picker
- Workspace skills can live in `.claude/skills/` next to the code

Global skills directory:

| Platform | Path |
|----------|------|
| macOS | `~/Library/Application Support/chatty/skills/` |
| Linux | `~/.local/share/chatty/skills/` |

## Related

- [Agentic tools](./agentic-tools.md)
- [Agents](./agents.md)
