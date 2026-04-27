# Agent Memory System

Chatty includes a persistent memory system that allows the AI agent to store and recall information across conversations and app restarts. It is built on [memvid-core](https://crates.io/crates/memvid-core), a lightweight vector database with hybrid similarity + full-text search.

## Overview

```
┌─────────────┐       ┌───────────────┐       ┌──────────────┐
│  LLM Agent  │──────▶│ MemoryService │──────▶│  memory.mv2  │
│  (rig-core) │◀──────│  (singleton)  │◀──────│  (on disk)   │
└─────────────┘       └───────────────┘       └──────────────┘
    uses tools:            Arc<Mutex<>>           binary file,
    RememberTool           thread-safe            persisted in
    SearchMemoryTool       async API              data directory
```

The agent has two tools:

| Tool | Purpose |
|:-----|:--------|
| `remember` | Store a piece of information with optional title and tags |
| `search_memory` | Retrieve relevant memories via natural-language query |

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

Search uses memvid-core's hybrid approach combining **vector similarity** and **full-text search** (enabled via the `lex` feature). Each result includes the stored text, optional title, and a relevance score.

### Automatic Recall

The system prompt instructs the agent to call `search_memory` on the **first user message of every conversation**, using a query derived from that message. This ensures relevant prior context is recalled without the user having to ask.

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
    ├── remember_tool.rs     # RememberTool (rig::Tool impl)
    └── search_memory_tool.rs # SearchMemoryTool (rig::Tool impl)
```

**`MemoryService`** — the core service, wraps `memvid-core` in `Arc<Mutex<>>` for async-safe access:

```rust
pub struct MemoryService {
    memvid: Arc<Mutex<Memvid>>,
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

**`MemoryHit`** — a single search result:

```rust
pub struct MemoryHit {
    pub text: String,
    pub title: Option<String>,
    pub score: f32,
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

This can be changed in the Settings → Execution panel. When disabled, the memory service is not initialized and the agent has no memory tools.

## Dependencies

```toml
memvid-core = { version = "2.0", default-features = false, features = ["lex"] }
```

The `lex` feature enables lexical (full-text) indexing alongside vector similarity search.
