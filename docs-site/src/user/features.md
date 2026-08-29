# Features

**When to read this:** Product capabilities beyond the agent loop.

## Multi-provider support

Connect multiple LLM providers from one interface. Per-model capabilities (vision, PDF, temperature) are stored in `ModelConfig`. See [Providers & models](./providers-and-models.md).

| Provider | Images | PDF | Temperature | Notes |
|----------|:------:|:---:|:-----------:|-------|
| OpenRouter | Per-model | Per-model | Yes | Routes to Anthropic, OpenAI, Google, Mistral, hundreds more |
| Azure OpenAI | Yes | Lossy | Yes | API Key or Entra ID |
| Ollama | Per-model | Per-model | — | Auto-detected, fully local |

## Rich rendering

- **Markdown** with full formatting
- **Syntax-highlighted code** (30+ languages via tree-sitter) with one-click copy
- **LaTeX math** — inline and block, rendered to SVG via Typst
- **Mermaid diagrams** — 23 diagram types, theme-aware, copy as PNG
- **Image and PDF** previews inline in chat

## Tool call traces

Every tool call is a collapsible trace block: name, arguments, output, duration, status. `apply_diff` calls show a visual diff (additions green, deletions red).

## Conversations & cost tracking

- Persistent SQLite storage (no Chatty-hosted sync)
- Auto-generated titles, sidebar search, export to Markdown
- Per-conversation cost in sidebar; per-message token usage
- Regeneration tracking creates DPO preference pairs

## Training data export

Export agent conversations for fine-tuning pipelines.

### ATIF (Agent Trajectory Interchange Format)

Structured JSON for agent training: messages, tool calls, reasoning, timestamps, token metrics, feedback, regeneration pairs. Compatible with [Harbor Framework](https://harborframework.com/docs/agents/trajectory-format) workflows.

### JSONL

- **SFT** — ChatML format for OpenAI, Anthropic, Together AI, etc.
- **DPO** — preference pairs from regenerated responses
- Auto-deduplication on re-export

| Platform | Export path |
|----------|-------------|
| macOS | `~/Library/Application Support/chatty/exports/` |
| Linux | `~/.config/chatty/exports/` |
| Windows | `%APPDATA%\chatty\exports\` |

Enable auto-export in **Settings → Training Data**.

## Environment secrets

**Settings → Secrets** — key-value pairs injected into every agent shell session. The agent knows variable names but never sees values.

## Themes & UI

20+ themes with light/dark variants. Configurable font size.

## Auto-updates

Background checks against GitHub releases with SHA-256 verification. macOS replaces the app bundle and relaunches; Linux refreshes bundled `chatty-tui` on next launch when installed via the desktop app.

## Demos

Animated demos live on the **[marketing site](https://github.com/boersmamarcel/chatty)** (`assets/animations/`). GIF placement in docs is pending human review (DOC-15).
