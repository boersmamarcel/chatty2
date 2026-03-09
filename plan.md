# Agent Memory Implementation Plan

## Overview

Add persistent, cross-conversation memory to Chatty's agentic system using [memvid-core](https://github.com/memvid/memvid). Memory is **global** (shared across all conversations), **agent-curated** (only stores what the agent explicitly decides to remember), and uses a **hybrid** interaction model (automatic retrieval + explicit tools).

All data stays **100% local** in a single `.mv2` file on disk.

## User Decisions

- **Scope**: Global (one memory store for the entire app)
- **Ingestion**: Agent-curated (agent decides what to remember via `remember` tool)
- **Interaction**: Hybrid (auto-retrieval of relevant context + explicit `remember`/`search_memory` tools)

---

## Step 1: Add `memvid-core` dependency

**File**: `Cargo.toml` (workspace root) + `crates/chatty-core/Cargo.toml`

Add `memvid-core` to workspace dependencies with minimal features:
```toml
# Workspace Cargo.toml
[workspace.dependencies]
memvid-core = { version = "2.0", features = ["lex", "vec"] }

# crates/chatty-core/Cargo.toml
memvid-core.workspace = true
```

Features needed:
- `lex` — Tantivy-based BM25 full-text search
- `vec` — HNSW vector search with ONNX embeddings

---

## Step 2: Memory service (`crates/chatty-core/src/services/memory_service.rs`)

A thin wrapper around `memvid-core::Memvid` that provides async-safe access.

```rust
pub struct MemoryService {
    memvid: Arc<Mutex<Memvid>>,
    path: PathBuf,
}

impl MemoryService {
    /// Open or create the global memory file
    pub async fn open_or_create(data_dir: &Path) -> Result<Self>;

    /// Store a memory entry with metadata
    pub async fn remember(&self, content: &str, tags: &[(&str, &str)]) -> Result<()>;

    /// Search memory by natural language query
    pub async fn search(&self, query: &str, top_k: usize) -> Result<Vec<MemoryHit>>;

    /// Get stats (frame count, file size)
    pub async fn stats(&self) -> Result<MemoryStats>;

    /// Clear all memory
    pub async fn clear(&self) -> Result<()>;
}

pub struct MemoryHit {
    pub text: String,
    pub title: Option<String>,
    pub score: f32,
    pub created_at: Option<String>,
}

pub struct MemoryStats {
    pub entry_count: usize,
    pub file_size_bytes: u64,
}
```

**Storage location** (follows existing math cache pattern):
- macOS: `~/Library/Application Support/chatty/memory.mv2`
- Linux: `~/.local/share/chatty/memory.mv2`
- Windows: `%APPDATA%\chatty\memory.mv2`

**Initialization**: Lazy — created on first use (first tool call or first auto-retrieval).

---

## Step 3: Global memory state (`crates/chatty-core/src/models/memory_store.rs`)

A lightweight global that holds the `MemoryService` and memory-enabled setting.

```rust
pub struct MemoryStore {
    service: Option<Arc<MemoryService>>,
    enabled: bool,
}

impl Global for MemoryStore {}
```

Initialized at startup alongside other globals in `main.rs`. The `MemoryService` itself is lazily created on first access.

---

## Step 4: Two new tools

### 4a: `RememberTool` (`crates/chatty-core/src/tools/remember_tool.rs`)

Allows the agent to explicitly store important information for future recall.

```
Tool name: "remember"
Description: "Store important information in persistent memory for future conversations.
Use this to save key facts, decisions, user preferences, project context, or anything
the user might want you to recall later. Be selective — only store genuinely useful information."

Parameters:
  - content (string, required): The information to remember
  - title (string, optional): Short title/label for the memory
  - tags (object, optional): Key-value tags for categorization (e.g., {"project": "chatty", "topic": "architecture"})
```

### 4b: `SearchMemoryTool` (`crates/chatty-core/src/tools/search_memory_tool.rs`)

Allows the agent to explicitly search its memory.

```
Tool name: "search_memory"
Description: "Search persistent memory for previously stored information. Use this when
you need to recall facts, decisions, or context from past conversations."

Parameters:
  - query (string, required): Natural language search query
  - top_k (integer, optional, default 5): Maximum number of results to return

Returns: JSON array of matching memory entries with text, title, score, and timestamp.
```

### 4c: Tool registration in `agent_factory.rs`

Add a new `MemoryTools` type tuple:
```rust
type MemoryTools = (RememberTool, SearchMemoryTool);
```

Gate on `memory_enabled` setting (new field in general settings). Always available when enabled — not gated on filesystem/execution settings since memory is the agent's own store.

---

## Step 5: Automatic context retrieval

**File**: `crates/chatty-core/src/services/llm_service.rs` (or `app_controller.rs` at the call site)

Before sending a prompt to the LLM, if memory is enabled:

1. Extract the user's latest message text
2. Call `memory_service.search(user_text, top_k=3)`
3. If results found, prepend a system-level context block:

```
[Relevant memories from past conversations]
- {memory_1.title}: {memory_1.text}
- {memory_2.title}: {memory_2.text}
[End of memories]
```

This is injected into the conversation's preamble/system message, NOT as a separate user message. This keeps it transparent to the LLM without polluting the conversation history.

**Performance**: memvid search is ~0.025ms P50, so this adds negligible latency.

---

## Step 6: Settings integration

### 6a: General settings model

Add to `GeneralSettingsModel`:
```rust
pub memory_enabled: bool,  // default: true
```

Persisted via the existing JSON settings repository.

### 6b: Settings UI (chatty-gpui only)

Add a "Memory" section to the settings view:
- Toggle: Enable/disable agent memory
- Stats display: "X memories stored (Y KB)"
- Button: "Clear Memory" (with confirmation)

---

## Step 7: Wire up initialization

**File**: `crates/chatty-gpui/src/main.rs` (or equivalent startup)

During app startup:
1. Load `GeneralSettingsModel` (already done)
2. If `memory_enabled`, initialize `MemoryStore` global with lazy service
3. The `MemoryService` creates/opens the `.mv2` file on first actual use

---

## File Summary

### New files:
| File | Purpose |
|------|---------|
| `crates/chatty-core/src/services/memory_service.rs` | Memvid wrapper service |
| `crates/chatty-core/src/models/memory_store.rs` | Global memory state |
| `crates/chatty-core/src/tools/remember_tool.rs` | `remember` tool |
| `crates/chatty-core/src/tools/search_memory_tool.rs` | `search_memory` tool |

### Modified files:
| File | Change |
|------|--------|
| `Cargo.toml` (workspace) | Add `memvid-core` workspace dep |
| `crates/chatty-core/Cargo.toml` | Add `memvid-core` dep |
| `crates/chatty-core/src/tools/mod.rs` | Export new tools |
| `crates/chatty-core/src/services/mod.rs` | Export memory service |
| `crates/chatty-core/src/models/mod.rs` | Export memory store |
| `crates/chatty-core/src/factories/agent_factory.rs` | Register memory tools |
| `crates/chatty-gpui/src/chatty/controllers/app_controller.rs` | Auto-retrieval before prompt |
| `crates/chatty-core/src/settings/models/` | Add `memory_enabled` setting |
| `crates/chatty-gpui/src/main.rs` | Initialize MemoryStore global |

---

## Implementation Order

1. Add dependency + memory service (Steps 1-2)
2. Memory store global (Step 3)
3. Tools: remember + search_memory (Step 4)
4. Wire tools into agent factory (Step 4c)
5. Settings model update (Step 6a)
6. Initialization wiring (Step 7)
7. Auto-retrieval (Step 5)
8. Settings UI (Step 6b)
9. Build + test
