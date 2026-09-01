# Memory & skills

Persistent facts and reusable procedures that survive across conversations.

## How memory works

Memory is a local binary file
([memvid-core](https://crates.io/crates/memvid-core)). Retrieval is full-text
by default. **Settings → Code Execution → Semantic Search** adds vector
similarity; that needs an embedding provider.

The agent calls `search_memory` when past context would help — facts,
preferences, earlier decisions, project conventions.

| Tool | What it does |
|------|--------------|
| `remember` | Store a fact, note, or preference with optional title and tags |
| `search_memory` | Retrieve memories with a natural-language query |

### Storage paths

| Platform | Path |
|----------|------|
| macOS | `~/Library/Application Support/chatty/memory.mv2` |
| Linux | `~/.local/share/chatty/memory.mv2` |
| Windows | `%APPDATA%\chatty\memory.mv2` |

Memory is on by default. Toggle it in **Settings → Code Execution**.

### Settings

- **Memory Browser** — browse, search, and delete entries in
  **Settings → Memory**
- **Purge All Memory** — permanently delete every stored memory
- **Semantic Search** — meaning-based retrieval when an embedding provider is
  configured

## Skills — reusable procedures

Skills are named, multi-step procedures for work you repeat:

- `save_skill` stores a procedure (for example `"deploy-to-staging"`)
- `search_memory` plus `read_skill` discover and load the instructions
- Skills show up in the `/` slash-command picker
- Workspace skills can live in `.claude/skills/` next to your code

Global skills directory:

| Platform | Path |
|----------|------|
| macOS | `~/Library/Application Support/chatty/skills/` |
| Linux | `~/.local/share/chatty/skills/` |
| Windows | `%APPDATA%\chatty\skills\` |

## Related

- [Agentic tools](./agentic-tools.md)
- [Agents](./agents.md)
