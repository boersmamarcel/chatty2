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
  - crates/chatty-core/src/settings/repositories/generic_json_repository.rs
  - crates/chatty-core/src/settings/repositories/oauth_credential_json_repository.rs
  - crates/chatty-core/src/settings/repositories/module_settings_json_repository.rs
  - crates/chatty-core/src/services/mcp_token_store.rs
  - crates/chatty-core/src/settings/models/
related:
  - ./env-vars.md
  - ./provider-matrix.md
  - ./singleton-inventory.md
  - ../adrs/settings-integration-map.md
---

# Settings schema reference

**When to read this:** Find the JSON file, Rust model, key fields, and defaults
for a persisted setting. Source of truth:
`crates/chatty-core/src/settings/repositories/` + `settings/models/`.

> **Pair review pending (DOC-23 / AGE-101):** Field tables below are transcribed
> from the settings models as of this commit. Marcel confirms file names,
> defaults, serde enum spellings, and that secret examples stay redacted
> before this page is treated as complete. Do not close AGE-101 until that
> review lands.

## Config vs data directories

JSON settings live under `dirs::config_dir()/chatty` (used by
`GenericJsonRepository`). WASM module **binaries** and agent memory live
under `dirs::data_dir()/chatty`.

| Kind | Resolver | Linux | macOS | Windows |
|------|----------|-------|-------|---------|
| Config (this page) | `dirs::config_dir()/chatty` | `~/.config/chatty/` (`$XDG_CONFIG_HOME/chatty`) | `~/Library/Application Support/chatty/` | `%APPDATA%\chatty\` |
| Data | `dirs::data_dir()/chatty` | `~/.local/share/chatty/` (`$XDG_DATA_HOME/chatty`) | `~/Library/Application Support/chatty/` | `%APPDATA%\chatty\` |

On macOS and Windows, config and data resolve to the same Application Support /
AppData folder. On Linux they differ (`~/.config` vs `~/.local/share`).

## Persistence behaviour

- **Single-object files** (`load` / `save`): missing file → `T::default()`.
- **List files** (`load_all` / `save_all`): missing file → `[]`.
- **Settings JSON writes** are atomic: pretty-printed JSON to a sibling
  `*.json.<pid>.tmp`, then rename (`GenericJsonRepository`).
- Serde field names are snake_case unless a table notes otherwise.
- `#[serde(skip)]` fields are runtime-only and never written to disk.

## File inventory

| File | Shape | Model | Repository |
|------|-------|-------|------------|
| `general_settings.json` | object | `GeneralSettingsModel` | `GeneralSettingsJsonRepository` |
| `execution_settings.json` | object | `ExecutionSettingsModel` | `ExecutionSettingsJsonRepository` |
| `search_settings.json` | object | `SearchSettingsModel` | `SearchSettingsJsonRepository` |
| `training_settings.json` | object | `TrainingSettingsModel` | `TrainingSettingsJsonRepository` |
| `user_secrets.json` | object | `UserSecretsModel` | `UserSecretsJsonRepository` |
| `hive_settings.json` | object | `HiveSettingsModel` | `HiveSettingsJsonRepository` |
| `extensions.json` | object | `ExtensionsModel` | `ExtensionsJsonRepository` |
| `module_settings.json` | object | `ModuleSettingsModel` | `ModuleSettingsJsonRepository` |
| `providers.json` | array | `ProviderConfig` | `JsonFileRepository` |
| `models.json` | array | `ModelConfig` | `JsonModelsRepository` |
| `mcp_servers.json` | array | `McpServerConfig` | `JsonMcpRepository` |
| `a2a_agents.json` | array | `A2aAgentConfig` | `A2aJsonRepository` |
| `mcp_oauth_<sanitized>.json` | object | opaque `StoredCredentials` JSON | live: `FileCredentialStore`; also `JsonOAuthCredentialRepository` |

Secrets in **bold** must never appear in logs, traces, or LLM-facing tool
output. Docs examples use `"****"` or omit the field.

---

## `general_settings.json` — `GeneralSettingsModel`

Source: `settings/models/general_model.rs`.

| Field | Type | Default | Notes |
|-------|------|---------|-------|
| `font_size` | `f32` | `14.0` | UI font size (px) |
| `theme_name` | `Option<String>` | `null` | Theme folder name; `null` = built-in default |
| `dark_mode` | `Option<bool>` | `null` | `null` = follow OS / theme default |

```json
{ "font_size": 14.0, "theme_name": null, "dark_mode": null }
```

---

## `execution_settings.json` — `ExecutionSettingsModel`

Source: `settings/models/execution_settings.rs`. Master toggle `enabled` is
opt-in (`false`) for security.

| Field | Type | Default | Notes |
|-------|------|---------|-------|
| `enabled` | `bool` | `false` | Master code-execution toggle |
| `approval_mode` | `ApprovalMode` | `"AlwaysAsk"` | JSON enum: `AlwaysAsk`, `AutoApproveSandboxed`, `AutoApproveAll` |
| `workspace_dir` | `Option<String>` | `null` | Absolute path required for filesystem / git tools |
| `filesystem_read_enabled` | `bool` | `true` | Requires `workspace_dir` |
| `filesystem_write_enabled` | `bool` | `true` | Requires `workspace_dir` + approval |
| `fetch_enabled` | `bool` | `true` | Built-in read-only HTTP GET |
| `git_enabled` | `bool` | `false` | Opt-in; workspace must be a git repo |
| `execute_code_enabled` | `bool` | `false` | Exposes `execute_code` to the model |
| `docker_code_execution_enabled` | `bool` | `false` | Docker fallback for non-Monty code |
| `docker_host` | `Option<String>` | `null` | Socket / URI; `null` = auto-detect |
| `timeout_seconds` | `u32` | `30` | Execution timeout |
| `max_output_bytes` | `usize` | `51200` | 50 KiB cap |
| `network_isolation` | `bool` | `false` | Sandbox network isolation when available |
| `max_agent_turns` | `u32` | `10` | Tool-call rounds per response |
| `memory_enabled` | `bool` | `true` | `remember` / `search_memory` |
| `embedding_enabled` | `bool` | `false` | Semantic memory search |
| `embedding_provider` | `Option<ProviderType>` | `null` | Independent of chat provider |
| `embedding_model` | `Option<String>` | `null` | e.g. `text-embedding-3-small` |

`ApprovalMode` has no `rename_all` — JSON uses the Rust variant names above.

---

## `search_settings.json` — `SearchSettingsModel`

Source: `settings/models/search_settings.rs`. Also stores Browser Use and
Daytona keys (not only web search).

| Field | Type | Default | Notes |
|-------|------|---------|-------|
| `enabled` | `bool` | `false` | Master web-search toggle |
| `active_provider` | `SearchProvider` | `"Tavily"` | JSON: `Tavily` or `Brave` |
| **`tavily_api_key`** | `Option<String>` | `null` | Secret |
| **`brave_api_key`** | `Option<String>` | `null` | Secret |
| `max_results` | `usize` | `5` | Search hit cap |
| `browser_use_enabled` | `bool` | `true` | Key alone is enough to activate; set `false` to disable without deleting the key |
| **`browser_use_api_key`** | `Option<String>` | `null` | Secret |
| `daytona_enabled` | `bool` | `true` | Same pattern as browser-use |
| **`daytona_api_key`** | `Option<String>` | `null` | Secret |

```json
{
  "enabled": false,
  "active_provider": "Tavily",
  "max_results": 5,
  "browser_use_enabled": true,
  "daytona_enabled": true
}
```

---

## `training_settings.json` — `TrainingSettingsModel`

Source: `settings/models/training_settings.rs`. Both flags opt-in.

| Field | Type | Default | Notes |
|-------|------|---------|-------|
| `atif_auto_export` | `bool` | `false` | ATIF JSON after each completed assistant turn |
| `jsonl_auto_export` | `bool` | `false` | JSONL (SFT + DPO) after each completed turn |

---

## `user_secrets.json` — `UserSecretsModel`

Source: `settings/models/user_secrets_store.rs`. Injected into shell sessions
as env vars; **never sent to the LLM**.

| Field | Type | Default | Persisted? |
|-------|------|---------|------------|
| **`secrets`** | `[{key, value}]` | `[]` | yes — values are secrets |
| `revealed_keys` | `HashSet<String>` | empty | **no** (`#[serde(skip)]`; UI-only) |

```json
{ "secrets": [ { "key": "EXAMPLE_TOKEN", "value": "****" } ] }
```

---

## `hive_settings.json` — `HiveSettingsModel`

Source: `settings/models/hive_settings.rs`.

| Field | Type | Default | Notes |
|-------|------|---------|-------|
| `registry_url` | `String` | `http://localhost:8080` | `DEFAULT_REGISTRY_URL` |
| `runner_url` | `String` | `http://localhost:8081` | `DEFAULT_RUNNER_URL` |
| **`token`** | `Option<String>` | `null` | JWT (30-day expiry). Secret. `is_logged_in()` = token present |
| `username` | `Option<String>` | `null` | Cached for UI |
| `email` | `Option<String>` | `null` | Cached for re-login |

---

## `extensions.json` — `ExtensionsModel`

Source: `settings/models/extensions_store.rs`. Unified install list for MCP,
WASM, and A2A.

| Field | Type | Default | Persisted? |
|-------|------|---------|------------|
| `extensions` | `[InstalledExtension]` | `[]` | yes |
| `mcp_auth_statuses` | map | empty | **no** (`#[serde(skip)]`) |
| `a2a_statuses` | map | empty | **no** (`#[serde(skip)]`) |

`InstalledExtension`:

| Field | Type | Default | Notes |
|-------|------|---------|-------|
| `id` | `String` | required | Unique slug |
| `display_name` | `String` | required | UI name |
| `description` | `String` | required | |
| `kind` | `ExtensionKind` | required | Internally tagged `kind`: `mcp` (flattened `McpServerConfig`), `wasm`, `a2a` (flattened `A2aAgentConfig`) |
| `source` | `ExtensionSource` | required | Internally tagged `type`: `hive` (`module_name`, `version`) or `custom` |
| `pricing_model` | `Option<String>` | `null` | Hive marketplace classification |
| `enabled` | `bool` | `true` | |

---

## `module_settings.json` — `ModuleSettingsModel`

Source: `settings/models/module_settings.rs`. Load path
(`ModuleSettingsJsonRepository`) runs `normalize_module_dir()`: empty values
and the legacy `.chatty/modules` fallback become the platform data-dir default.

| Field | Type | Default | Notes |
|-------|------|---------|-------|
| `enabled` | `bool` | `false` | WASM module runtime |
| `module_dir` | `String` | platform data dir + `/chatty/modules` | Linux `~/.local/share/chatty/modules`; macOS `~/Library/Application Support/chatty/modules`; Windows `%APPDATA%\chatty\modules`. Last-resort fallback: `.chatty/modules` |
| `gateway_port` | `u16` | `8420` | Local protocol gateway |

---

## `providers.json` — `[ProviderConfig]`

Source: `settings/models/providers_store.rs`. See also
[Provider matrix](./provider-matrix.md).

| Field | Type | Default | Notes |
|-------|------|---------|-------|
| `name` | `String` | required | Display name |
| `provider_type` | `ProviderType` | required | JSON: `open_router`, `ollama`, `azure_openai`. Serde `rename_all = "snake_case"` on the variant `OpenRouter` yields `open_router` — the string `openrouter` **does not** deserialize. Legacy aliases `open_ai`, `open_a_i`, `anthropic`, `gemini`, `mistral` deserialize as OpenRouter |
| **`api_key`** | `Option<String>` | omitted if `null` | Secret. Never expose to the LLM |
| `base_url` | `Option<String>` | omitted if `null` | Ollama / Azure / OpenAI-compat |
| `extra_config` | `Map<String, String>` | `{}` (omitted if empty) | Azure: `auth_method` = `api_key` (default) or `entra_id` |

`configured_providers()`: Ollama always; Azure needs non-empty `base_url` plus
API key or Entra ID; others need a non-empty API key.

---

## `models.json` — `[ModelConfig]`

Source: `settings/models/models_store.rs`.

| Field | Type | Default | Notes |
|-------|------|---------|-------|
| `id` | `String` | required | Internal UUID (not the API model name) |
| `name` | `String` | required | UI label |
| `provider_type` | `ProviderType` | required | Same enum as providers (`open_router` / `ollama` / `azure_openai`) |
| `model_identifier` | `String` | required | API / Ollama model id |
| `temperature` | `f32` | `1.0` | |
| `preamble` | `String` | `""` | System prompt / GEPA target |
| `max_tokens` | `Option<i32>` | omitted if `null` | |
| `top_p` | `Option<f32>` | omitted if `null` | |
| `extra_params` | `Map<String, String>` | `{}` (omitted if empty) | Azure: `api_version` (default `2025-03-01-preview`) |
| `cost_per_million_input_tokens` | `Option<f64>` | omitted if `null` | USD |
| `cost_per_million_output_tokens` | `Option<f64>` | omitted if `null` | USD |
| `supports_images` | `bool` | `false` | New models inherit `ProviderType::default_capabilities()` |
| `supports_pdf` | `bool` | `false` | |
| `supports_temperature` | `bool` | `true` | Off for some reasoning models |
| `max_context_window` | `Option<i32>` | omitted if `null` | Token-bar budget |

---

## `mcp_servers.json` — `[McpServerConfig]`

Source: `settings/models/mcp_store.rs`. HTTP MCP endpoints only (the app does
not spawn stdio servers). Writes are serialized with `MCP_WRITE_LOCK`.

| Field | Type | Default | Notes |
|-------|------|---------|-------|
| `name` | `String` | required | Unique id |
| `url` | `String` | required | e.g. `http://localhost:3000/mcp` |
| **`api_key`** | `Option<String>` | omitted if `null` | Bearer token. Secret. LLM-facing copies must not include the real value |
| `enabled` | `bool` | `true` | |
| `is_module` | `bool` | `false` (omitted when false) | Auto-registered WASM gateway entry |

Runtime `McpAuthStatus` is not persisted.

`MASKED_API_KEY_SENTINEL` is `"****"`: on edit, that string means “keep the
stored key”. There is no `env` map on `McpServerConfig` in current source.

---

## `a2a_agents.json` — `[A2aAgentConfig]`

Source: `settings/models/a2a_store.rs`.

| Field | Type | Default | Notes |
|-------|------|---------|-------|
| `name` | `String` | required | Also the `/agent <name>` token |
| `url` | `String` | required | A2A base URL |
| **`api_key`** | `Option<String>` | omitted if `null` | Bearer token. Secret |
| `enabled` | `bool` | `true` | |
| `skills` | `[String]` | `[]` (omitted if empty) | Cached from the remote agent card |

Runtime `A2aAgentStatus` is not persisted.

---

## `mcp_oauth_<sanitized>.json` — OAuth credentials

One file per MCP server name in the **config** dir. Non-alphanumeric
characters in the server name (except `-` `_`) become `_`.
Example: server `my server/with:special.chars` →
`mcp_oauth_my_server_with_special_chars.json`.

The **live writer** is `FileCredentialStore` (`mcp_token_store.rs`), used by
`McpService`. `JsonOAuthCredentialRepository` uses the same filenames but is
not registered in `init_repositories()`. `FileCredentialStore` writes the
JSON directly (not the temp-file rename used by settings repos).

Shape is opaque `rmcp::StoredCredentials` JSON (`client_id`,
`token_response`, `granted_scopes`, …). **Do not paste real token files
into issues, PRs, or docs.** Corrupt files are deleted on load.

---

## Not persisted as JSON (yet)

| Store | Typical path | Notes |
|-------|--------------|-------|
| `TokenTrackingSettings` | in-memory GPUI global | Defaults: `enabled` `true`, `response_reserve` `4096`, `high_threshold` `0.70`, `critical_threshold` `0.90`, `auto_summarize` `false`, `summarization_model_id` omitted. Source comment: JSON persistence is a follow-up. |
| Conversations | config dir `chatty/conversations.db` | SQLite, not settings JSON |
| Agent memory / skills | data dir `chatty/` | memvid + skill files |
| WASM module binaries | data dir `chatty/modules/` | See `module_dir` |
| Pdfium native lib | data dir `chatty/lib/` | See [env-vars](./env-vars.md) |
| Math SVG cache | data dir `chatty/math_cache/` | |

Planned research settings (`FlowSettingsModel`, playbook store) are **not
implemented** — see [settings integration map](../adrs/settings-integration-map.md).

`openrouter_curated.json` also lives in the config dir (GPUI OpenRouter catalog
override: `[{id, name}, …]`). It is **not** a settings repository.

## Related

- [Settings integration map](../adrs/settings-integration-map.md) — research ↔ settings
- [Environment variables](./env-vars.md) — `CHATTY_*` / `XDG_*`
- [Provider matrix](./provider-matrix.md) — auth and default capabilities
- [Singleton inventory](./singleton-inventory.md) — repository accessors
EOF

# Fail docs-gen if a repository JSON filename is missing from the schema page.
python3 - "$ROOT/crates/chatty-core/src/settings/repositories" "$OUT/settings-schema.md" << 'PY'
import re, sys
from pathlib import Path

repos_dir, schema = map(Path, sys.argv[1:3])
filenames = set()
for path in repos_dir.glob("*.rs"):
    filenames.update(re.findall(r'"([^"]+\.json)"', path.read_text()))
schema_text = schema.read_text()
missing = sorted(f for f in filenames if f not in schema_text)
# Format strings like mcp_oauth_{sanitized}.json still count if the prefix is documented.
missing = [
    f
    for f in missing
    if not (f.startswith("mcp_oauth_") and "mcp_oauth_" in schema_text)
]
if missing:
    print("settings-schema.md missing repository files:", ", ".join(missing), file=sys.stderr)
    sys.exit(1)
print("settings-schema: checked", len(filenames), "repository JSON filenames")
PY

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
- Persisted `ProviderType` JSON is `open_router` (serde snake_case). Legacy values `open_ai`, `open_a_i`, `anthropic`, `gemini`, `mistral` deserialize as OpenRouter. The string `openrouter` is **not** accepted.
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

## How-to guides

- [Add a provider](${SITE_BASE}/dev/guides/add-provider.html)
- [Add a tool](${SITE_BASE}/dev/guides/add-tool.html)
- [Add a slash command](${SITE_BASE}/dev/guides/add-slash-command.html)
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
append_section "GPUI event catalog" "$OUT/event-catalog.md"
append_section "Singleton inventory" "$OUT/singleton-inventory.md"
append_section "Environment variables" "$OUT/env-vars.md"
append_section "Settings schema" "$OUT/settings-schema.md"
append_section "Where do I…?" "$ROOT/docs-site/src/dev/where-to-look.md"

echo "gen-docs-reference: wrote reference pages to $OUT"
