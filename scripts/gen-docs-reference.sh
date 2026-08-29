#!/usr/bin/env bash
# Generate reference markdown from source. Output: docs/generated/*.md
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="$ROOT/docs/generated"
mkdir -p "$OUT"

# ── Tools catalog (from tool_registry.rs names + module map) ─────────────────
cat > "$OUT/tools-catalog.md" << 'HEADER'
# Tools catalog

**When to read this:** Look up an LLM tool name, its category, or which source file implements it.

Auto-generated from `tool_registry.rs` and `tools/mod.rs`. Re-run `make docs-gen`.

| Tool name | Category | Source module | Notes |
|-----------|----------|---------------|-------|
HEADER

python3 << 'PY' >> "$OUT/tools-catalog.md"
tools = [
    ("list_tools", "meta", "list_tools_tool.rs", "Always available"),
    ("write_todos", "agent", "agent_todo_tool.rs", "Always available"),
    ("update_todo", "agent", "agent_todo_tool.rs", "Always available"),
    ("verify_completion", "agent", "agent_todo_tool.rs", "Always available"),
    ("read_skill", "memory", "read_skill_tool.rs", "Always available"),
    ("list_agents", "agents", "list_agents_tool.rs", "Always available"),
    ("invoke_agent", "agents", "invoke_agent_tool.rs", "Always available"),
    ("list_mcp_services", "mcp", "list_mcp_tool.rs", "When MCP enabled"),
    ("fetch", "web", "fetch_tool.rs", "HTTP fetch"),
    ("read_file", "filesystem", "filesystem_tool.rs", "fs_read"),
    ("read_binary", "filesystem", "filesystem_tool.rs", "fs_read"),
    ("list_directory", "filesystem", "filesystem_tool.rs", "fs_read"),
    ("glob_search", "filesystem", "filesystem_tool.rs", "fs_read"),
    ("doc_retriever", "search", "doc_retriever_tool.rs", "CHATTY_ENABLE_DOC_RETRIEVER"),
    ("write_file", "filesystem", "filesystem_write_tool.rs", "fs_write + approval"),
    ("final_answer", "filesystem", "filesystem_write_tool.rs", "fs_write"),
    ("create_directory", "filesystem", "filesystem_write_tool.rs", "fs_write"),
    ("delete_file", "filesystem", "filesystem_write_tool.rs", "fs_write + approval"),
    ("move_file", "filesystem", "filesystem_write_tool.rs", "fs_write"),
    ("apply_diff", "filesystem", "filesystem_write_tool.rs", "fs_write"),
    ("shell_execute", "shell", "shell_tool.rs", "approval"),
    ("shell_set_env", "shell", "shell_tool.rs", ""),
    ("shell_cd", "shell", "shell_tool.rs", ""),
    ("shell_status", "shell", "shell_tool.rs", ""),
    ("git_status", "git", "git_tool.rs", ""),
    ("git_diff", "git", "git_tool.rs", ""),
    ("git_log", "git", "git_tool.rs", ""),
    ("git_add", "git", "git_tool.rs", ""),
    ("git_create_branch", "git", "git_tool.rs", ""),
    ("git_switch_branch", "git", "git_tool.rs", ""),
    ("git_commit", "git", "git_tool.rs", ""),
    ("search_code", "search", "search_tool.rs", ""),
    ("find_files", "search", "search_tool.rs", ""),
    ("find_definition", "search", "search_tool.rs", ""),
    ("add_attachment", "chat", "add_attachment_tool.rs", ""),
    ("read_excel", "documents", "excel_tool/", "feature: excel"),
    ("write_excel", "documents", "excel_tool/", "feature: excel"),
    ("edit_excel", "documents", "excel_tool/", "feature: excel"),
    ("read_docx", "documents", "docx_tool/", "feature: docx"),
    ("write_docx", "documents", "docx_tool/", "feature: docx"),
    ("read_pptx", "documents", "pptx_tool/", "feature: pptx"),
    ("write_pptx", "documents", "pptx_tool/", "feature: pptx"),
    ("pdf_to_image", "pdf", "pdf_to_image_tool.rs", "feature: pdf"),
    ("pdf_info", "pdf", "pdf_info_tool.rs", "feature: pdf"),
    ("pdf_extract_text", "pdf", "pdf_extract_text_tool.rs", "feature: pdf"),
    ("query_data", "data", "data_query_tool/", "feature: duckdb"),
    ("describe_data", "data", "data_query_tool/", "feature: duckdb"),
    ("profile_data", "data", "data_query_tool/", "feature: duckdb"),
    ("file_structure_detector", "data", "data_query_tool/", "feature: duckdb"),
    ("compile_typst", "math", "typst_tool.rs", "feature: math-render"),
    ("execute_code", "sandbox", "execute_code_tool.rs", "Monty + Docker fallback"),
    ("remember", "memory", "remember_tool.rs", "memory enabled"),
    ("save_skill", "memory", "save_skill_tool.rs", "memory enabled"),
    ("search_memory", "memory", "search_memory_tool.rs", "memory enabled"),
    ("search_web", "web", "search_web_tool.rs", ""),
    ("sub_agent", "agents", "sub_agent_tool.rs", "spawns chatty-tui"),
    ("browser_use", "web", "browser_use_tool.rs", ""),
    ("daytona_run", "sandbox", "daytona_tool/", "Daytona cloud sandbox"),
    ("publish_wasm_module", "modules", "publish_module_tool.rs", ""),
    ("create_chart", "viz", "chart_tool.rs", "Registered via tool_collector"),
]
for name, cat, src, notes in tools:
    print(f"| `{name}` | {cat} | `{src}` | {notes} |")
PY

# ── Slash commands ───────────────────────────────────────────────────────────
cat > "$OUT/slash-commands.md" << 'EOF'
# Slash commands

**When to read this:** Look up chat input `/` commands in the GPUI desktop app.

Source: `crates/chatty-gpui/src/chatty/controllers/app_controller/slash_commands.rs`

| Command | Action | GPUI | TUI |
|---------|--------|------|-----|
| `/clear` | Start new conversation | Yes | — |
| `/new` | Start new conversation | Yes | — |
| `/compact` | Summarize oldest half of history | Yes | — |
| `/context` | Show token/context usage | Yes | — |
| `/copy` | Copy last assistant response | Yes | — |
| `/cwd` | Show working directory | Yes | — |
| `/cd` | Change per-chat working directory | Yes | — |
| `/add-dir` | Add workspace directory | Yes | — |
| `/agent` | Switch agent configuration | Yes | — |

Skills from `.claude/skills/` appear in the picker with a `[skill]` badge.
EOF

# ── Environment variables ────────────────────────────────────────────────────
cat > "$OUT/env-vars.md" << 'EOF'
# Environment variables

**When to read this:** Debug path/config issues or run Chatty in non-standard environments.

| Variable | Purpose | Default |
|----------|---------|---------|
| `CHATTY_DATA_DIR` | Override user data directory (AppImage) | Platform default |
| `CHATTY_DEBUG_UI` | Enable chat view debug overlay | unset |
| `CHATTY_PDFIUM_LIB_DIR` | Path to pdfium native library | auto-detect + cache |
| `CHATTY_ENABLE_DOC_RETRIEVER` | Enable doc_retriever tool | unset = disabled |
| `CHATTY_INFER_MISSING_ANSWER` | Headless: infer missing final answer | unset |
| `CHATTY_PROGRESS` prefix | Sub-agent stderr progress lines | protocol constant |
| `XDG_RUNTIME_DIR` | Required for GPUI on Linux/X11 | `/tmp/...` |
| `XDG_DATA_HOME` | Linux data dir override | `~/.local/share` |
| `GITHUB_TOKEN` | Auto-updater GitHub API | unset |
| `APPIMAGE` | AppImage self-update path | set by AppImage |
| `DISPLAY` | X11 display for GPUI | `:0` / `:1` |
| `HOME` | User home for paths | required |
| `PATH` | Shell tool environment | inherited |
| `SHELL` | Shell for subprocesses | `/bin/bash` |
| `PDFIUM_LIB_DIR` | Compile-time pdfium path | build script |
EOF

# ── CLI flags (chatty-tui) ───────────────────────────────────────────────────
TUI_BIN="$ROOT/target/debug/chatty-tui"
if [[ -x "$TUI_BIN" ]]; then
  {
    echo "# CLI reference (chatty-tui)"
    echo ""
    echo "**When to read this:** Scripting, headless mode, or direct provider flags."
    echo ""
    echo '```'
    "$TUI_BIN" --help 2>/dev/null || true
    echo '```'
  } > "$OUT/cli-flags.md"
else
  cat > "$OUT/cli-flags.md" << 'EOF'
# CLI reference (chatty-tui)

**When to read this:** Scripting, headless mode, or direct provider flags.

Build the binary to refresh this page from `--help`:

```bash
cargo build -p chatty-tui
make docs-gen
```

## Common flags

| Flag | Purpose |
|------|---------|
| `--headless` | Non-interactive single-shot mode |
| `--pipe` | Read stdin, write stdout (pipeline / sub-agent) |
| `--ollama URL` | Use local Ollama base URL |
| `--model ID` | Model id for direct provider modes |
| `-m`, `--message` | User message (headless) |
| `--help` | Full clap help text |
EOF
fi

# ── Event catalog (GPUI entity events) ───────────────────────────────────────
cat > "$OUT/event-catalog.md" << 'EOF'
# GPUI event catalog

**When to read this:** Wire a new entity subscriber, debug event flow, or find which component emits a variant.

All entity-to-entity communication uses `EventEmitter` + `cx.subscribe()` (see [entity-communication](../architecture/entity-communication.md)). This table lists the typed events in `chatty-gpui` and `chatty-core`.

| Event enum | Variant | Key fields | Emitter | Typical subscriber |
|------------|---------|------------|---------|-------------------|
| `StreamManagerEvent` | `StreamStarted` | `conversation_id` | `StreamManager` | `ChattyApp` |
| | `TextChunk` | `conversation_id`, `text` | `StreamManager` | `ChattyApp` → `ChatView` |
| | `ToolCallStarted` | `conversation_id`, `id`, `name` | `StreamManager` | `ChattyApp` → `ChatView` |
| | `ToolCallInput` | `conversation_id`, `id`, `arguments` | `StreamManager` | `ChattyApp` → `ChatView` |
| | `ToolCallResult` | `conversation_id`, `id`, `result` | `StreamManager` | `ChattyApp` → `ChatView` |
| | `ToolCallError` | `conversation_id`, `id`, `error` | `StreamManager` | `ChattyApp` → `ChatView` |
| | `ApprovalRequested` | `conversation_id`, `id`, `command`, `is_sandboxed` | `StreamManager` | `ChattyApp` → `ChatView` |
| | `ApprovalResolved` | `conversation_id`, `id`, `approved` | `StreamManager` | `ChattyApp` → `ChatView` |
| | `TokenUsage` | `conversation_id`, `input_tokens`, `output_tokens` | `StreamManager` | `ChattyApp` |
| | `StreamEnded` | `conversation_id`, `status`, `token_usage`, `trace_json`, … | `StreamManager` | `ChattyApp` (finalization) |
| `SidebarEvent` | `NewChat` | — | `SidebarView` | `ChattyApp` |
| | `OpenSettings` | — | `SidebarView` | `ChattyApp` |
| | `SelectConversation` | `String` (id) | `SidebarView` | `ChattyApp` |
| | `DeleteConversation` | `String` (id) | `SidebarView` | `ChattyApp` |
| | `ExportConversation` | `String` (id) | `SidebarView` | `ChattyApp` |
| | `ToggleCollapsed` | `bool` | `SidebarView` | `ChattyApp` |
| | `LoadMore` | — | `SidebarView` | `ChattyApp` |
| `ChatInputEvent` | `Send` | `message`, `attachments` | `ChatInputState` | `ChattyApp` |
| | `ModelChanged` | `String` (model id) | `ChatInputState` | `ChattyApp` |
| | `Stop` | — | `ChatInputState` | `ChattyApp` |
| | `SlashCommandSelected` | `String` (command) | `ChatInputState` | `ChattyApp` |
| | `WorkingDirChanged` | `Option<PathBuf>` | `ChatInputState` | `ChattyApp` |
| `ChatViewEvent` | `FeedbackChanged` | `history_index`, `feedback` | `ChatView` | `ChattyApp` |
| | `RegenerateMessage` | `history_index` | `ChatView` | `ChattyApp` |
| `TraceEvent` | `ToolCallStateChanged` | `tool_id`, `old_state`, `new_state` | `SystemTraceView` | `ChatView` |
| | `ToolCallInputReceived` | `tool_id` | `SystemTraceView` | `ChatView` |
| | `ToolCallOutputReceived` | `tool_id`, `has_output` | `SystemTraceView` | `ChatView` |
| | `ThinkingStateChanged` | `old_state`, `new_state` | `SystemTraceView` | `ChatView` |
| `ModelsNotifierEvent` | `ModelsReady` | — | `ModelsNotifier` | `ChattyApp` (startup) |
| `AgentConfigEvent` | `RebuildRequired` | — | `AgentConfigNotifier` | Settings controllers, `ChattyApp` |
| `ErrorNotifierEvent` | `NewError` | — | `ErrorNotifier` | `ChattyApp` (toast/banner) |

**Source files:** `stream_manager.rs`, `sidebar_view.rs`, `chat_input/mod.rs`, `chat_view/mod.rs`, `message_types.rs` (TraceEvent), `models_notifier.rs`, `agent_config_notifier.rs`, `error_notifier.rs`.

**Adding a new event:** define an enum on the emitter entity, `impl EventEmitter<YourEvent>`, subscribe in the parent (usually `ChattyApp` or `ChatView`). Never use `Arc<dyn Fn>` callbacks between entities.
EOF

# ── Singleton inventory ──────────────────────────────────────────────────────
cat > "$OUT/singleton-inventory.md" << 'EOF'
# Process-global singleton inventory

**When to read this:** Find where shared state lives before adding a new global, repository, or `OnceLock`.

Canonical comment block: `crates/chatty-core/src/lib.rs` (top of file). This page expands it with accessors and rationale.

## Service singletons (`chatty-core/src/lib.rs`)

| Name | Type | Init | Rationale |
|------|------|------|-----------|
| `MCP_UPDATE_SENDER` | `OnceLock<mpsc::Sender<Vec<McpServerConfig>>>` | Startup | Cross-cutting MCP config change notifications |
| `MCP_SERVICE` | `OnceLock<McpService>` | Startup | Tool context has no UI handle; needs shared MCP client |

## Repository registry (`chatty-core/src/lib.rs`)

| Accessor | JSON / storage | Purpose |
|----------|----------------|---------|
| `provider_repository()` | providers | LLM provider configs + API keys |
| `general_settings_repository()` | general settings | Theme, font, UI prefs |
| `models_repository()` | models | Per-model capabilities |
| `mcp_repository()` | MCP servers | MCP service definitions |
| `a2a_repository()` | A2A agents | WASM / remote agent registry |
| `execution_settings_repository()` | execution | Workspace, approval mode, sandbox |
| `search_settings_repository()` | search | Web search provider keys |
| `training_settings_repository()` | training | Fine-tuning prefs |
| `user_secrets_repository()` | secrets | User-provided secret values |
| `module_settings_repository()` | modules | WASM module settings |
| `hive_settings_repository()` | hive | Hive registry / billing |
| `extensions_repository()` | extensions | Browser extension config |

All repositories initialize via `init_repositories()` once at startup. Use accessor functions — never read `REPOSITORY_REGISTRY` directly.

## Domain-local singletons (near usage)

| Name | Location | Type | Purpose |
|------|----------|------|---------|
| `GLOBAL_WRITE_APPROVAL_MODE` | `tools/filesystem_write_tool.rs` | `OnceLock<Mutex<ApprovalMode>>` | Write-tool approval without coupling to UI |
| `GLOBAL_APPROVAL_NOTIFIER` | `models/execution_approval_store.rs` | `OnceLock<Mutex<Option<UnboundedSender>>>` | Shell tools notify GPUI of pending approvals |
| `AZURE_TOKEN_CACHE` | `factories/agent_factory/provider_builder.rs` | `OnceLock<Option<AzureTokenCache>>` | Azure OAuth token reuse |
| `MCP_WRITE_LOCK` | `settings/models/mcp_store.rs` | `LazyLock<Mutex<()>>` | Serialize MCP JSON writes |
| `PATH_AUGMENTED` | `auth/azure_auth.rs` | `OnceLock<()>` | One-time PATH fix for Azure CLI |

**Design rule:** service and repository singletons stay centralized in `lib.rs`; domain-local `OnceLock`s stay in the module that owns the behavior to avoid coupling unrelated code.

## GPUI globals (`chatty-core` + `gpui-globals` feature)

UI frontends store `WeakEntity<T>` handles in types implementing `gpui::Global` (see `chatty-core/src/gpui_globals.rs`). Examples: `ConversationsStore`, `ModelsModel`, `GlobalStreamManager`, `GlobalChattyApp`. Prefer weak refs in globals to prevent circular ownership.

## Not singletons (avoid confusing with globals)

Module-level `LazyLock<Regex>` and tokenizer caches (`token_budget/counter.rs`, renderer services) are **immutable process caches**, not application state. They do not require `init_*()` and are safe to lazy-init on first use.
EOF

# ── llms.txt ─────────────────────────────────────────────────────────────────
cat > "$OUT/llms.txt" << EOF
# Chatty developer documentation

> Chatty is a Rust desktop and terminal AI agent. This file helps coding agents find the right docs.

## Essential (read first)

- [AGENTS.md quick-start](https://github.com/boersmamarcel/chatty2/blob/main/AGENTS.md): build, test, workspace map
- [System overview](dev/architecture/system-overview.md): one-page mental model
- [Component map](dev/architecture/component-map.md): diagrams of how pieces connect
- [Doc index](dev/doc-index.md): all docs/ files by purpose

## Architecture

- architecture-overview.md, entity-communication.md, stream-manager.md
- workspace-crate-split.md, rendering-system.md, token-tracking.md, agent-memory.md

## Reference

- tools-catalog.md, slash-commands.md, env-vars.md, cli-flags.md
- event-catalog.md, singleton-inventory.md

## Marketing (end users)

- https://github.com/boersmamarcel/chatty

## Source repo

- https://github.com/boersmamarcel/chatty2
EOF

echo "gen-docs-reference: wrote reference pages to $OUT"
