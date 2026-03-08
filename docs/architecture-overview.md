# Architecture Overview

A high-level guide to Chatty's module structure, data flow, and key design decisions for contributors.

## Workspace Structure

Chatty is organized as a Cargo workspace with two crates:

```
Cargo.toml                       # Workspace root
crates/
├── chatty-core/                 # UI-agnostic: models, services, tools, settings
│   └── src/
│       ├── lib.rs               # Crate root, global singletons (repositories, MCP)
│       ├── gpui_globals.rs      # impl Global for core types (behind gpui-globals feature)
│       ├── auth/                # Azure AD authentication (OAuth2 flows, token cache)
│       ├── exporters/           # Conversation export (ATIF, JSONL/SFT/DPO)
│       ├── factories/           # LLM agent construction (per-provider)
│       ├── models/              # Domain entities and global stores
│       ├── repositories/        # Persistence layer (SQLite, in-memory)
│       ├── sandbox/             # Docker sandbox for code execution
│       ├── services/            # Business logic services
│       ├── settings/            # Settings models, repositories, provider discovery
│       ├── token_budget/        # Context window management (counting, summarization)
│       └── tools/               # LLM tool implementations
│
└── chatty-gpui/                 # GPUI desktop frontend
    └── src/
        ├── main.rs              # Entry point: Tokio runtime, theme loading, global init
        ├── assets.rs            # Embedded assets (icons, fonts)
        ├── auto_updater/        # Self-update system (download, install, UI)
        ├── chatty/
        │   ├── controllers/     # Application logic hub (ChattyApp entity)
        │   ├── models/          # GPUI-specific models (StreamManager, ErrorNotifier)
        │   ├── services/        # GPUI-specific services
        │   ├── token_budget/    # GPUI token budget manager (UI integration)
        │   └── views/           # UI components (GPUI Render impls)
        └── settings/
            ├── controllers/     # Settings CRUD operations
            ├── models/          # GPUI-specific notifiers (ModelsNotifier, AgentConfigNotifier)
            ├── providers/       # Provider-specific GPUI services (Ollama sync)
            ├── utils/           # Theme and window helpers
            └── views/           # Settings UI pages
```

See [workspace-crate-split.md](workspace-crate-split.md) for full rationale and design patterns.

## Data Flow

### Startup Sequence

```
main.rs
 ├── Create Tokio runtime
 ├── Load theme from ./themes/
 ├── Initialize globals:
 │   ├── GeneralSettingsModel (font size, theme prefs)
 │   ├── ProviderModel (API providers)
 │   ├── ModelsModel (available LLM models)
 │   ├── McpService (MCP server manager)
 │   └── ConversationsStore (conversation metadata)
 ├── Async load from disk:
 │   ├── Settings JSON files → update globals
 │   ├── Conversation metadata (SQLite) → populate sidebar
 │   └── MCP servers → start enabled servers
 └── Create window → ChattyApp entity
```

### Message Send Flow

```
User types message → ChatInputState
   │
   ├── cx.emit(ChatInputEvent::Submit)
   │
   ▼
ChattyApp::handle_chat_input_event()
   │
   ├── 1. Create/get conversation (ConversationsStore)
   ├── 2. Build agent (AgentFactory → provider-specific client)
   ├── 3. Register stream in StreamManager
   ├── 4. Spawn async task:
   │      ├── stream_prompt() → ResponseStream
   │      └── Loop: StreamChunk → StreamManager::handle_chunk()
   │
   ▼
StreamManager emits StreamManagerEvent
   │
   ├── TextChunk → ChatView appends text
   ├── ToolCallStarted → ChatView shows tool UI
   ├── StreamEnded → Finalization (title gen, save, sidebar refresh)
   └── ...
```

### Persistence Architecture

```
Runtime (globals)              Persistence
─────────────────              ───────────
GeneralSettingsModel  ←──JSON──  general_settings.json
ProviderModel         ←──JSON──  providers.json
ModelsModel           ←──JSON──  models.json
McpStore              ←──JSON──  mcp_servers.json
ExecutionSettings     ←──JSON──  execution_settings.json
TrainingSettings      ←──JSON──  training_settings.json
UserSecretsStore      ←──JSON──  user_secrets.json
ConversationsStore    ←─SQLite─  conversations.db
TokenTrackingSettings ←──JSON──  token_tracking.json
```

Settings use the **optimistic update pattern**: update the global immediately for instant UI feedback, then save to disk asynchronously.

## Key Design Decisions

### 1. Central Controller Pattern

`ChattyApp` (`app_controller.rs`, ~3000 lines) is the central hub. It:
- Owns all top-level view entities (sidebar, chat view, input)
- Subscribes to events from child entities
- Coordinates between services, stores, and views

This is intentionally a "fat controller" rather than distributed logic, to keep the event flow traceable. It's the largest file and a candidate for future splitting.

### 2. Event-Driven Communication

All entity-to-entity communication uses GPUI's `EventEmitter`/`cx.subscribe()`. No `Arc<dyn Fn>` callbacks between entities. See [entity-communication.md](entity-communication.md) for rationale and topology.

### 3. StreamManager Owns Stream Lifecycle

LLM response streams are managed by a centralized `StreamManager` entity using cancellation tokens and typed events. The stream loop never directly updates the UI — it emits events that handlers route to views. See [stream-manager.md](stream-manager.md).

### 4. Global State via GPUI

Application-wide state uses GPUI's `Global` trait + `cx.set_global()`/`cx.global()`. Entity references in globals use `WeakEntity<T>` to avoid circular references.

### 5. Provider Abstraction

LLM providers (Anthropic, OpenAI, Gemini, Ollama, Mistral, Azure) are abstracted through:
- `ProviderType` enum with `default_capabilities()` for initialization defaults
- `AgentFactory` that builds provider-specific clients
- `ModelConfig` for per-model persisted capabilities

### 6. Tool System

Tools are Rig framework tool implementations that the LLM can invoke during conversations:

| Tool | Purpose |
|------|---------|
| `shell_tool` | Execute shell commands (with approval flow) |
| `filesystem_tool` | Read files and directories |
| `filesystem_write_tool` | Create/edit files (with approval flow) |
| `search_tool` | Search file contents with regex |
| `git_tool` | Git operations |
| `fetch_tool` | HTTP requests |
| `add/edit/delete/list_mcp_tool` | Manage MCP servers |
| `list_tools_tool` | List available tools |
| `add_attachment_tool` | Add file attachments |

Tools requiring side effects (shell, file writes) go through an approval flow via `ExecutionApprovalStore` / `WriteApprovalStore`.

### 7. Token Budget Management

The `token_budget/` module manages context window limits:
- **Counter**: Estimates token counts per message
- **Manager**: Tracks total budget and triggers summarization
- **Summarizer**: Compresses older messages when budget is exceeded
- **Cache**: Caches token counts to avoid re-computation
- **Snapshot**: Read-only view of budget state for UI display

See [token-tracking.md](token-tracking.md) for data flow details.

## Module Dependencies

```
chatty-gpui                          chatty-core
───────────                          ───────────
views ──────► controllers ──────►    services
  │               │                     │
  │               ▼                     ▼
  │           models (GPUI)         models/stores (core)
  │               │                     │
  │               ▼                     ▼
  │           StreamManager         repositories
  │                                     │
  └─────────────────────────────────────┘
                                        │
                                    factories
```

**chatty-core** (no GPUI dependency):
- **Models/Stores** hold global state (conversations, settings, approvals)
- **Services** contain business logic (LLM streaming, MCP, shell, math rendering)
- **Repositories** handle disk I/O (JSON files, SQLite)
- **Factories** construct provider-specific LLM clients
- **Tools** implement LLM-callable tool definitions

**chatty-gpui** (depends on chatty-core with `gpui-globals` feature):
- **Views** render UI and emit events; they never call services directly
- **Controllers** handle events, coordinate between services and stores
- **Models** hold GPUI-specific state (StreamManager, ErrorNotifier, notifiers)

## Adding a New Feature — Checklist

1. **New setting?** → Add model in `chatty-core/src/settings/models/`, add JSON persistence in `chatty-core/src/settings/repositories/`, add UI in `chatty-gpui/src/settings/views/`
2. **New LLM provider?** → Add variant to `ProviderType` in chatty-core, update `default_capabilities()`, add agent builder in `chatty-core/src/factories/agent_factory.rs`
3. **New tool?** → Implement in `chatty-core/src/tools/`, register in `agent_factory.rs`
4. **New view component?** → Add in `chatty-gpui/src/chatty/views/`, emit events to `ChattyApp` if it needs to trigger actions
5. **New service?** → If UI-agnostic, add in `chatty-core/src/services/`. If GPUI-specific, add in `chatty-gpui/src/chatty/services/`
6. **New global type?** → Define in chatty-core, add `impl Global` in `chatty-core/src/gpui_globals.rs` (behind `gpui-globals` feature)
