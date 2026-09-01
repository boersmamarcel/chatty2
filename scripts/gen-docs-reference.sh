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

**When to read this:** Look up chat input `/` commands. GPUI and TUI do not share a command table — see [Add a slash command](../guides/add-slash-command.md).

Sources: `crates/chatty-gpui/src/chatty/views/chat_input/slash.rs`,
`crates/chatty-gpui/src/chatty/controllers/app_controller/slash_commands.rs`,
`crates/chatty-tui/src/engine/commands.rs`.

| Command | Action | GPUI | TUI |
|---------|--------|------|-----|
| `/clear` | Start new conversation | Yes | Yes |
| `/new` | Start new conversation | Yes | Yes |
| `/compact` | Summarize oldest half of history | Yes | Yes |
| `/context` | Show token/context usage | Yes | Yes |
| `/copy` | Copy last assistant response | Yes | Yes |
| `/cwd` | Show working directory | Yes | Yes |
| `/cd [dir]` | Change per-chat working directory | Yes | Yes |
| `/add-dir <dir>` | Add workspace directory | Yes | Yes |
| `/agent [name] <prompt>` | Launch local sub-agent or named A2A agent | Yes | Yes |
| `/model [query]` | Switch / list models | — | Yes |
| `/tools [name]` | Open tool picker or toggle by name | — | Yes |
| `/modules …` | Module runtime settings | — | Yes |
| `/update` | CLI auto-update | — | Yes |
| `/quit` `/exit` | Quit the application | — | Yes |

Skills from `.claude/skills/` appear in both pickers with a skill badge.
EOF

# ── Settings schema (AGE-101, pair review) ──────────────────────────────────
cat > "$OUT/settings-schema.md" << 'EOF'
---
audience: [contributor, agent]
source_files:
  - crates/chatty-core/src/settings/repositories/mod.rs
  - crates/chatty-core/src/settings/models/
related:
  - ./dev/reference/env-vars.md
  - ./dev/adrs/settings-integration-map.md
---

# Settings schema reference

**When to read this:** Find the JSON file, model, and defaults for a persisted
setting.

> **Pair review pending (DOC-23 / AGE-101):** Tables below are generated from
> `settings/repositories/` + `settings/models/` as of this commit. Marcel
> confirms file names, defaults, and which secrets must never appear in docs
> examples before this page is treated as complete.

## Config directory

`dirs::config_dir()/chatty` — via `generic_json_repository::chatty_config_dir()`.

| Platform | Typical path |
|----------|----------------|
| Linux | `~/.config/chatty/` (`$XDG_CONFIG_HOME/chatty`) |
| macOS | `~/Library/Application Support/chatty/` (`dirs::config_dir`) |
| Windows | `%APPDATA%\chatty\` |

Missing files load `Default`. Saves are atomic (temp file + rename).

Module **binaries** (WASM) live under `dirs::data_dir()/chatty/modules/`, not
the config dir — see `ModuleSettingsModel`.

## Single-object files (`load` / `save`)

| File | Model | Key fields / defaults |
|------|-------|------------------------|
| `general_settings.json` | `GeneralSettingsModel` | `font_size` `14.0`; `theme_name` / `dark_mode` `None` |
| `execution_settings.json` | `ExecutionSettingsModel` | `enabled` `false`; `approval_mode` `AlwaysAsk`; `workspace_dir` `None`; filesystem + fetch default **on**; git / execute_code / docker default **off**; `timeout_seconds` `30`; `max_output_bytes` `51200`; `max_agent_turns` `10`; `memory_enabled` `true` |
| `search_settings.json` | `SearchSettingsModel` | `enabled` `false`; `active_provider` `Tavily`; API keys `None`; `max_results` `5` |
| `training_settings.json` | `TrainingSettingsModel` | `atif_auto_export` / `jsonl_auto_export` `false` |
| `user_secrets.json` | `UserSecretsModel` | secret key/value list — **do not log or paste real values** |
| `hive_settings.json` | `HiveSettingsModel` | `registry_url` `http://localhost:8080`; `runner_url` `http://localhost:8081`; `token` `None` |
| `extensions.json` | `ExtensionsModel` | enabled A2A / extension entries |
| `module_settings.json` | `ModuleSettingsModel` | `enabled` `false`; `gateway_port` `8420`; `module_dir` = platform data dir (`~/Library/Application Support/chatty/modules` on macOS, `~/.local/share/chatty/modules` on Linux) |

OAuth tokens: `mcp_oauth_<sanitized_name>.json` in the same config dir
(`oauth_credential_json_repository`).

## List files (`load_all` / `save_all`)

| File | Item type | Notes |
|------|-----------|--------|
| `providers.json` | `ProviderConfig` | API keys live here; never expose to the LLM |
| `models.json` | `ModelConfig` | per-model capabilities, preamble, temperature |
| `mcp_servers.json` | `McpServerConfig` | use `masked_env()` on any LLM-facing copy |
| `a2a_agents.json` | `A2aAgentConfig` | registered A2A agents |

## Not persisted as JSON (yet)

| Model | Notes |
|-------|-------|
| `TokenTrackingSettings` | In-memory global; defaults `enabled` `true`, reserve `4096`, high `0.70`, critical `0.90`, `auto_summarize` `false`. Comment in source says JSON persistence is a follow-up. |

## Related

- [Settings integration map](../adrs/settings-integration-map.md) — research ↔ settings
- [Environment variables](./env-vars.md) — `CHATTY_*` / `XDG_*`
EOF

# ── Provider matrix ─────────────────────────────────────────────────────────
cat > "$OUT/provider-matrix.md" << 'EOF'
# Provider matrix

**When to read this:** Configure LLM backends, auth, or default model capabilities.

Source: `crates/chatty-core/src/settings/models/providers_store.rs`,
`crates/chatty-core/src/factories/agent_factory/provider_builder.rs`.

## Provider types

| Provider | Display name | Auth | Required config | Default images | Default PDF | Temperature default |
|----------|--------------|------|-----------------|----------------|-------------|---------------------|
| `openrouter` | OpenRouter | API key | `api_key`; optional `base_url` | yes | yes | per `ModelConfig` |
| `ollama` | Ollama | none | optional `base_url` (default `http://localhost:11434`) | no | no | always applied |
| `azure_openai` | Azure OpenAI | API key **or** Entra ID | `base_url` (resource endpoint) + credentials | yes | no | per `ModelConfig` |

### OpenRouter

- Gateway to 200+ upstream models (Anthropic, OpenAI, Google, Mistral, Meta, …).
- Legacy JSON values `open_ai`, `anthropic`, `gemini`, `mistral` deserialize as `openrouter` for backward compatibility.
- `configured_providers()` requires a non-empty `api_key`.
- Rig client: `rig_core::providers::openrouter::Client`.
- MCP tools are sanitized for OpenAI-compatible schemas before attachment.

### Ollama

- Local or remote Ollama instance; no API key required.
- Included in `configured_providers()` even without `api_key`.
- Default capabilities are `(false, false)`; vision/PDF support is detected per model via `/api/show` (`sync_service.rs`) and stored in `ModelConfig`.
- Rig client: `rig_core::providers::ollama::Client`.

### Azure OpenAI

- Auth via `extra_config.auth_method`: `api_key` (default) or `entra_id`.
- `configured_providers()` requires non-empty `base_url` **and** (`api_key` **or** Entra ID).
- Endpoint URLs are normalized (trailing slashes stripped, `/openai/deployments/...` paths truncated).
- Per-model `api_version` in `ModelConfig.extra_params`; default from `AZURE_DEFAULT_API_VERSION`.
- Entra ID tokens are cached in `AzureTokenCache` when available.
- Rig client: `rig_core::providers::azure::Client`.

## Model capabilities (`ModelConfig`)

| Field | Purpose |
|-------|---------|
| `supports_images` | Allow image attachments in chat |
| `supports_pdf` | Allow PDF attachments |
| `supports_temperature` | Show temperature slider; skipped for reasoning models |

New models inherit `ProviderType::default_capabilities()`. Ollama overrides per model after sync.

## TUI direct-connect flags (bypass Settings UI)

| Flag | Maps to | Notes |
|------|---------|-------|
| `--ollama [URL]` | Ollama provider | Auto-discovers models from `/api/tags` |
| `--openai-compat-url URL` | OpenRouter provider type | vLLM, llama.cpp, LM Studio, etc.; optional `--api-key` |

See [CLI flags](./cli-flags.md) for full `chatty-tui` options.
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
# Prefer a live `--help` dump when the binary is already built. Docs CI does
# not compile chatty-tui (18+ minutes); keep an existing generated page or
# write a static fallback so `make docs` still works without a Rust toolchain.
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
elif [[ -f "$OUT/cli-flags.md" ]]; then
  echo "gen-docs-reference: chatty-tui not built; keeping existing cli-flags.md"
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

**Adding a new event:** define an enum on the emitter entity, `impl EventEmitter<YourEvent>`, subscribe in the parent (usually `ChattyApp` or `ChatView`). Never use `Arc<dyn Fn>` callbacks between entities. Step-by-step: [Add a desktop GPUI view](../guides/add-gpui-view.md).
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

# ── llms.txt (llmstxt.org curated index) ─────────────────────────────────────
SITE_BASE="https://boersmamarcel.github.io/chatty2"
cat > "$OUT/llms.txt" << EOF
# Chatty developer documentation

> Chatty is a Rust desktop and terminal AI agent (GPUI + Ratatui). Curated links for coding agents.

## Essential

- [Agent quick-start (AGENTS.md)](${SITE_BASE}/dev/agents.html): build, test, workspace map, conventions
- [Where do I…? decision tree](${SITE_BASE}/dev/where-to-look.html): task → file/doc routing
- [Documentation index](${SITE_BASE}/dev/doc-index.html): all docs/ files by purpose
- [System overview](${SITE_BASE}/dev/architecture/system-overview.html): one-page mental model
- [Component map](${SITE_BASE}/dev/architecture/component-map.html): crate/entity diagrams

## Architecture

- [Architecture overview](${SITE_BASE}/dev/architecture/architecture-overview.html)
- [Entity communication](${SITE_BASE}/dev/architecture/entity-communication.html)
- [Stream manager](${SITE_BASE}/dev/architecture/stream-manager.html)
- [Workspace crate split](${SITE_BASE}/dev/architecture/workspace-crate-split.html)
- [Rendering system](${SITE_BASE}/dev/architecture/rendering-system.html)
- [Token tracking](${SITE_BASE}/dev/architecture/token-tracking.html)
- [Agent memory](${SITE_BASE}/dev/architecture/agent-memory.html)

## Reference

- [Tools catalog](${SITE_BASE}/dev/reference/tools-catalog.html)
- [Provider matrix](${SITE_BASE}/dev/reference/provider-matrix.html)
- [Slash commands](${SITE_BASE}/dev/reference/slash-commands.html)
- [CLI flags](${SITE_BASE}/dev/reference/cli-flags.html)
- [Environment variables](${SITE_BASE}/dev/reference/env-vars.html)
- [Settings schema](${SITE_BASE}/dev/reference/settings-schema.html)
- [GPUI event catalog](${SITE_BASE}/dev/reference/event-catalog.html)
- [Singleton inventory](${SITE_BASE}/dev/reference/singleton-inventory.html)

## User guides

- [Getting started](${SITE_BASE}/user/getting-started.html)
- [Agents](${SITE_BASE}/user/agents.html)
- [Agentic tools](${SITE_BASE}/user/agentic-tools.html)
- [Terminal interface](${SITE_BASE}/user/terminal.html)

## How-to guides

- [Add a provider](${SITE_BASE}/dev/guides/add-provider.html)
- [Add a tool](${SITE_BASE}/dev/guides/add-tool.html)
- [Add a slash command](${SITE_BASE}/dev/guides/add-slash-command.html)
- [Add a desktop GPUI view](${SITE_BASE}/dev/guides/add-gpui-view.html)
- [Debug streams](${SITE_BASE}/dev/guides/debug-streams.html)
- [Build & package](${SITE_BASE}/dev/guides/build-package.html)

## Research / reserved symbols

- [RESERVED.md](https://github.com/boersmamarcel/chatty2/blob/main/RESERVED.md): human-only functions in research crates
- [App ↔ research bridge](${SITE_BASE}/dev/adrs/app-research-bridge.html)

## Optional

- [Full context bundle](${SITE_BASE}/llms-full.txt): concatenated key pages for large-context agents
- [Marketing site](https://github.com/boersmamarcel/chatty): product marketing and extra demos
- [Source repo](https://github.com/boersmamarcel/chatty2)
EOF

# ── llms-full.txt (concatenated key pages) ───────────────────────────────────
LLMS_FULL="$OUT/llms-full.txt"
: > "$LLMS_FULL"

append_section() {
  local title="$1"
  local file="$2"
  [[ -f "$file" ]] || return 0
  {
    echo ""
    echo "================================================================================"
    echo "# $title"
    echo "# Source: ${file#$ROOT/}"
    echo "================================================================================"
    echo ""
    cat "$file"
    echo ""
  } >> "$LLMS_FULL"
}

append_section "AGENTS.md" "$ROOT/AGENTS.md"
append_section "Documentation index" "$ROOT/docs/INDEX.md"
append_section "System overview" "$ROOT/docs/system-overview.md"
append_section "Entity communication" "$ROOT/docs/entity-communication.md"
append_section "Stream manager" "$ROOT/docs/stream-manager.md"
append_section "Provider matrix" "$OUT/provider-matrix.md"
append_section "Tools catalog" "$OUT/tools-catalog.md"
append_section "GPUI event catalog" "$OUT/event-catalog.md"
append_section "Singleton inventory" "$OUT/singleton-inventory.md"
append_section "Environment variables" "$OUT/env-vars.md"
append_section "Settings schema" "$OUT/settings-schema.md"
append_section "Where do I…?" "$ROOT/docs-site/src/dev/where-to-look.md"

echo "gen-docs-reference: wrote reference pages to $OUT"
