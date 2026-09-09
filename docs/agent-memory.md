# Agent Memory System

**When to read this:** You are touching the `remember` / `search_memory` / `save_skill` tools or the `MemoryService` behind them.

Chatty includes a persistent memory system that allows the AI agent to store and recall information across conversations and app restarts. It is built on [memvid-core](https://crates.io/crates/memvid-core), a lightweight vector database with hybrid similarity + full-text search.

## Overview

```
┌─────────────┐       ┌───────────────┐       ┌──────────────┐
│  LLM Agent  │──────▶│ MemoryService │──────▶│  memory.mv2  │
│  (rig-core) │◀──────│  (singleton)  │◀──────│  (on disk)   │
└─────────────┘       └───────────────┘       └──────────────┘
    uses tools:            dedicated memvid       binary file,
    RememberTool           thread behind an       persisted in
    SearchMemoryTool       async command API      data directory
    SaveSkillTool
```

The agent has three tools backed by this store:

| Tool | Purpose |
|:-----|:--------|
| `remember` | Store a piece of information with optional title and tags |
| `search_memory` | Retrieve relevant memories via natural-language query |
| `save_skill` | Store a reusable multi-step procedure as a memory entry |

## Storage

Memories are stored in a single binary `.mv2` file (memvid format) in the platform-specific data directory:

| Platform | Path |
|:---------|:-----|
| Linux | `~/.local/share/chatty/memory.mv2` (or `$XDG_DATA_HOME/chatty/`) |
| macOS | `~/Library/Application Support/chatty/memory.mv2` |
| Windows | `%APPDATA%\chatty\memory.mv2` |

The file is created lazily on the first `remember` call and committed to disk after every write.

## How It Works

### Storing Memories

The `RememberTool` accepts:

- **`content`** (required) — the information to store
- **`title`** (optional) — a short label (e.g., "User prefers dark mode")
- **`tags`** (optional) — key-value metadata for categorization (e.g., `{"project": "chatty", "topic": "ui"}`)

Content is stored as UTF-8 bytes with attached metadata via `memvid-core`'s `put_bytes_with_options()`.

### Searching Memories

The `SearchMemoryTool` accepts:

- **`query`** (required) — a natural-language search string
- **`top_k`** (optional) — max results to return (default: 5, range: 1–20)

Search uses memvid-core's full-text index (the `lex` feature). When `embedding_enabled` is on and an embedding provider is configured, `remember` also stores an embedding (`remember_with_embedding`) and `search_memory` queries the vector index (`search_vec`) instead. Each result includes the stored text, optional title, and a relevance score.

### Automatic Recall

The system prompt instructs the agent to call `search_memory` proactively whenever a question might benefit from stored context (preferences, prior decisions, project conventions), and to call `remember` — not just say "noted" — whenever the user asks it to remember something.

## Architecture

### Initialization

1. At app startup, the `memory_enabled` setting is checked (enabled by default)
2. If enabled, `MemoryService::open_or_create()` is called asynchronously
3. The service is stored as a **global singleton** via `cx.set_global()`
4. When an agent is created (via `AgentFactory`), the memory tools are conditionally injected only if the service exists

### Key Types

```
crates/chatty-core/src/
├── services/
│   └── memory_service.rs    # MemoryService, MemoryHit, MemoryStats
└── tools/
    ├── remember_tool.rs     # RememberTool (rig_agent::tool::Tool impl)
    └── search_memory_tool.rs # SearchMemoryTool (rig_agent::tool::Tool impl)
```

**`MemoryService`** — the core service. It owns a dedicated OS thread that performs every memvid operation (open, index setup, search, put, commit) and talks to it over a command channel, so the async executor is never blocked and tantivy's thread-sensitive index state stays on one thread:

```rust
#[derive(Clone)]
pub struct MemoryService {
    cmd_tx: mpsc::Sender<MemoryCommand>,
    path: PathBuf,
}
```

Public API:

| Method | Description |
|:-------|:------------|
| `open_or_create(data_dir)` | Open existing or create new `.mv2` store |
| `remember(content, title, tags)` | Store a memory entry |
| `search(query, top_k)` | Search memories by natural language |
| `stats()` | Get entry count and file size |
| `clear()` | Remove all stored memories |
| `delete(frame_id)` / `list_memories(query, limit)` | Used by the Settings → Memory browser |
| `remember_with_embedding()` / `search_vec()` | Vector variants used when semantic search is enabled |

**`MemoryHit`** — a single search result:

```rust
pub struct MemoryHit {
    pub text: String,
    pub title: Option<String>,
    pub score: f32,                    // serialised as `relevance_score`
    pub source: Option<MemoryHitSource>,
    pub frame_id: Option<u64>,         // memvid frame id, not shown to the LLM
}
```

### Graceful Degradation

- Searching an empty store returns an empty result set (no errors)
- If memory initialization fails, the agent simply runs without memory tools
- The tools are only registered when `MemoryService` is available

## Configuration

Memory is toggled via the `memory_enabled` field in `ExecutionSettingsModel`:

```rust
pub struct ExecutionSettingsModel {
    /// Enable persistent agent memory (remember/search_memory tools).
    #[serde(default = "default_true")]
    pub memory_enabled: bool,
}
```

This can be changed in the Settings → Memory page. When disabled, the memory service is not initialized and the agent has no memory tools.

## Research connection (ACE / M4)

The memory + `[SKILL]` split is the production substrate for [ACE playbooks](research/modules/m4-ace.md).
`chatty-playbook` will add Reflector/Curator delta ops and deterministic merge on top of this
store — see the [app ↔ research bridge](research/app-research-bridge.md#memory-skills--playbook-m4).

## Dependencies

```toml
memvid-core = { version = "2.0.139", default-features = false, features = ["lex", "temporal_track"] }
```

The `lex` feature enables lexical (full-text) indexing; `temporal_track` is enabled for memvid's temporal search request options, which Chatty currently leaves unset.
