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

## How-to guides

- [Add a provider](${SITE_BASE}/dev/guides/add-provider.html)
- [Add a tool](${SITE_BASE}/dev/guides/add-tool.html)
- [Debug streams](${SITE_BASE}/dev/guides/debug-streams.html)
- [Build & package](${SITE_BASE}/dev/guides/build-package.html)

## Research / reserved symbols

- [RESERVED.md](https://github.com/boersmamarcel/chatty2/blob/main/RESERVED.md): human-only functions in research crates
- [App ↔ research bridge](${SITE_BASE}/dev/adrs/app-research-bridge.html)

## Optional

- [Full context bundle](${SITE_BASE}/llms-full.txt): concatenated key pages for large-context agents
- [Marketing site](https://github.com/boersmamarcel/chatty): end-user README and screenshots
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
append_section "Environment variables" "$OUT/env-vars.md"
append_section "Where do I…?" "$ROOT/docs-site/src/dev/where-to-look.md"

echo "gen-docs-reference: wrote reference pages to $OUT"
