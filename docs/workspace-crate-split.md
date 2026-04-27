# Workspace Crate Split

Rationale, design patterns, and guidelines for Chatty's three-crate workspace architecture.

## Why Three Crates?

The codebase is organized into three workspace crates:

| Crate | Purpose | GPUI dependency? |
|:------|:--------|:-----------------|
| **chatty-core** | Models, services, tools, settings, repositories, factories | No |
| **chatty-gpui** | Desktop UI: views, controllers, stream manager, notifiers | Yes |
| **chatty-tui** | Terminal UI: single-session chat, headless/pipe mode | No |

**Benefits:**
- **Testability**: chatty-core's 500+ unit and 14 integration tests run without a display server or GPU — fast CI, no headless hacks
- **Reusability**: chatty-core powers both frontends (GPUI desktop and Ratatui terminal) without duplication
- **Compile-time boundaries**: prevents accidental UI coupling in business logic
- **Faster incremental builds**: changes to views don't recompile the core
- **Sub-agent ready**: chatty-tui's headless mode enables future programmatic LLM usage

## Dependency Direction

```
chatty-gpui ──depends on──► chatty-core (with gpui-globals feature)
chatty-tui  ──depends on──► chatty-core (without gpui-globals)
```

chatty-core never depends on either frontend crate. GPUI-specific integration is opt-in via the `gpui-globals` feature flag. chatty-tui does not use this feature — it uses chatty-core's services and models directly without GPUI globals.

## The `gpui-globals` Feature

chatty-core types (stores, settings models, services) need to implement GPUI's `Global` trait so chatty-gpui can use `cx.set_global()` / `cx.global()`. This is handled via a conditional module:

```toml
# chatty-core/Cargo.toml
[features]
gpui-globals = ["gpui"]

[dependencies]
gpui = { workspace = true, optional = true }
```

```rust
// chatty-core/src/lib.rs
#[cfg(feature = "gpui-globals")]
mod gpui_globals;

// chatty-core/src/gpui_globals.rs
use gpui::Global;

impl Global for crate::settings::models::GeneralSettingsModel {}
impl Global for crate::settings::models::ModelsModel {}
impl Global for crate::models::ConversationsStore {}
// ... all types that need cx.set_global()
```

chatty-gpui always enables this feature; chatty-tui does not:

```toml
# chatty-gpui/Cargo.toml
chatty-core = { path = "../chatty-core", features = ["gpui-globals"] }

# chatty-tui/Cargo.toml
chatty-core = { path = "../chatty-core" }  # No gpui-globals
```

### Adding a New Global Type

1. Define the type in chatty-core (e.g., `src/models/my_store.rs`)
2. Add `impl Global for crate::models::MyStore {}` in `src/gpui_globals.rs`
3. Use `cx.set_global(MyStore::new())` in chatty-gpui as usual

## What Lives Where

### chatty-core owns:

| Module | Contents |
|:-------|:---------|
| `models/` | `ConversationsStore`, `ErrorStore`, `ExecutionApprovalStore`, `WriteApprovalStore`, `AttachmentValidation`, `MessageTypes`, `TokenUsage` |
| `services/` | `LlmService`, `McpService`, `MathRendererService`, `MermaidRendererService`, `ShellService`, `FilesystemService`, `GitService`, `SearchService`, `ChartSvgRenderer`, `TitleGenerator`, `TypstCompilerService`, `ErrorCollectorLayer`, `PdfThumbnail`, `PdfiumUtils`, `PathValidator` |
| `tools/` | All LLM-callable tools (shell, filesystem, git, search, fetch, MCP management, PDF, chart, etc.) |
| `factories/` | `AgentFactory` — builds provider-specific LLM clients |
| `settings/models/` | `GeneralSettingsModel`, `ModelsModel`, `ProviderModel`, `McpServersModel`, `ExecutionSettingsModel`, `TrainingSettingsModel`, `TokenTrackingSettings`, `UserSecretsModel` |
| `settings/repositories/` | All JSON file and SQLite persistence |
| `settings/providers/` | Provider-specific discovery (Ollama model discovery) |
| `repositories/` | `ConversationRepository`, `ConversationSqliteRepository`, `InMemoryRepository` |
| `auth/` | Azure AD OAuth2 flows, token cache |
| `exporters/` | ATIF and JSONL conversation export |
| `sandbox/` | Docker sandbox for code execution |
| `token_budget/` | Token counting, snapshots, caching, summarization |

### chatty-gpui owns:

| Module | Contents |
|:-------|:---------|
| `main.rs` | Entry point, Tokio runtime, theme loading, global initialization |
| `assets.rs` | Embedded icons, fonts |
| `auto_updater/` | Self-update download, install, UI |
| `chatty/controllers/` | `ChattyApp` — central controller entity |
| `chatty/models/` | `StreamManager` (stream lifecycle), `ErrorNotifier` |
| `chatty/services/` | GPUI-specific services |
| `chatty/token_budget/` | Token budget manager (GPUI entity wrapping core snapshots) |
| `chatty/views/` | All UI components (chat view, sidebar, input, message rendering, etc.) |
| `settings/controllers/` | Settings CRUD operations (create/update/delete models, providers, etc.) |
| `settings/models/` | `ModelsNotifier`, `AgentConfigNotifier` (GPUI event emitters) |
| `settings/providers/` | Ollama sync service (GPUI-integrated) |
| `settings/utils/` | Theme and window helpers |
| `settings/views/` | Settings UI pages |

### chatty-tui owns:

| Module | Contents |
|:-------|:---------|
| `main.rs` | Entry point, CLI args (clap), Tokio runtime, settings loading, model resolution |
| `app.rs` | Ratatui render loop, crossterm input, `tokio::select!` event multiplexing |
| `engine.rs` | `ChatEngine` — single-conversation orchestrator, stream processing, approval flow |
| `events.rs` | `AppEvent` enum — channel-based events (replaces GPUI's `EventEmitter`) |
| `headless.rs` | Headless mode (single message → stdout) and pipe mode (stdin → stdout) |
| `ui/` | Ratatui widgets: chat view, text input, status bar, approval prompt |

### How chatty-tui differs from chatty-gpui

| Aspect | chatty-gpui | chatty-tui |
|:-------|:------------|:-----------|
| UI framework | GPUI (GPU-accelerated) | Ratatui (terminal) |
| Conversations | Multi-conversation, SQLite persistence | Single session, in-memory only |
| Event system | `EventEmitter` / `cx.subscribe()` | `tokio::mpsc` channels (`AppEvent`) |
| Stream management | `StreamManager` entity with typed events | `ChatEngine` handles events directly |
| State management | GPUI globals (`cx.set_global()`) | Owned fields on `ChatEngine` |
| Non-interactive | N/A | `--headless` and `--pipe` modes |

### Decision Criteria

Ask: **"Does this code need GPUI's `Context`, `Window`, `Entity`, `Render`, or `EventEmitter`?"**

- **Yes** → chatty-gpui
- **No, but it's terminal UI or TUI-specific logic** → chatty-tui
- **No** → chatty-core

## Re-exports in chatty-gpui

chatty-gpui re-exports core modules so existing controller code continues to work without path changes:

```rust
// chatty-gpui/src/chatty/mod.rs
pub use chatty_core::auth;
pub use chatty_core::exporters;
pub use chatty_core::factories;
pub use chatty_core::repositories;
pub use chatty_core::tools;

// GPUI-specific modules (not in core)
pub mod controllers;
pub mod models;
pub mod services;
pub mod token_budget;
pub mod views;
```

## Integration Testing Strategy

### chatty-core integration tests (`crates/chatty-core/tests/integration.rs`)

Test the public API surface from an external consumer's perspective:
- Settings model CRUD lifecycle
- ConversationsStore operations and ordering
- Provider capability propagation to ModelConfig
- Azure provider configuration filtering
- JSON serialization roundtrips
- Token budget snapshot calculations

These run without any display server — ideal for CI.

### chatty-gpui integration tests (`crates/chatty-gpui/tests/core_integration.rs`)

Verify core types work correctly when compiled with the `gpui-globals` feature:
- All global types remain functional with `impl Global`
- Cross-crate type interactions (settings + conversations wired together)
- Token budget snapshots usable from the GPUI crate

These require linking GPUI but don't need a live display connection.

## Build Commands

```bash
# Build entire workspace
cargo build

# Build only the TUI
cargo build -p chatty-tui

# Test only chatty-core (no display needed)
cargo test -p chatty-core

# Test only chatty-gpui (requires X11/Wayland libs)
cargo test -p chatty-gpui

# Test everything
cargo test --workspace
```
