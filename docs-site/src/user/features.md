# Features

**When to read this:** Product capabilities beyond the agent loop.

## Multi-provider support

Connect several LLM backends in one window. Per-model vision, PDF, and
temperature flags drive the UI. Table and setup:
[Providers & models](./providers-and-models.md).

## Rich rendering

- **Markdown** with full formatting
- **Syntax-highlighted code** (30+ languages via tree-sitter) and one-click copy

  ![Syntax-highlighted code blocks](../assets/animations/codehighlighting.gif)

- **LaTeX math** — inline (`$...$`) and block (`$$...$$`) compiled to SVG via Typst

  ![LaTeX math rendering](../assets/animations/advanced_math_rendering.gif)

- **Mermaid diagrams** — fenced mermaid code blocks as inline SVG, theme-aware
  dark/light, copy source or PNG. 23 diagram types (flowcharts, sequence, ER,
  Gantt, …) via a pure-Rust renderer; no browser required

  ![Mermaid diagram rendering](../assets/animations/mermaid.gif)

- **Image and PDF** previews in chat

## Tool-call traces

Each tool call is a collapsible block: name, arguments, output, duration,
status (success / error / cancelled). `apply_diff` gets a visual diff
(additions green, deletions red, collapsed unchanged runs, “Show N more lines”
on large patches).

## Conversations & cost

- Conversations persist in local SQLite (no Chatty-hosted sync)
- Auto-generated titles; title-bar search filters the sidebar live
- **Export to Markdown** from the conversation `…` menu
- Per-conversation cost in the sidebar; per-message input/output tokens and cost
- Pricing uses the model's configured cost per million input/output tokens
- Regenerating an assistant reply captures the original as a DPO pair

![Token and cost tracking](../assets/animations/advanced_token_tracking.gif)

## Training-data export

**Settings → Training Data** can auto-export runs for fine-tuning.

### ATIF (Agent Trajectory Interchange Format)

Structured JSON for agent pipelines ([Harbor trajectory format](https://harborframework.com/docs/agents/trajectory-format)):
messages, tool calls, reasoning, timestamps, token metrics, thumbs feedback,
and regeneration pairs (rejected vs chosen).

### JSONL

- **SFT** — ChatML for OpenAI, Anthropic, Together AI, and similar APIs
- **DPO** — preference pairs from regenerations
- Re-export replaces the previous entry for that conversation
- Tool calls can be included in ChatML

SFT appends to `sft.jsonl`, DPO to `dpo.jsonl`:

| Platform | Path |
|----------|------|
| macOS | `~/Library/Application Support/chatty/exports/` |
| Linux | `~/.config/chatty/exports/` |
| Windows | `%APPDATA%\chatty\exports\` |

## Environment secrets

**Settings → Secrets** — key-value pairs injected into every agent shell
session. The agent sees names (`os.environ["API_KEY"]`) but never values.
Secrets persist locally and are masked in tool output.

## Themes & UI

20+ themes with light and dark variants (Ayu, Catppuccin, Everforest, Flexoki,
Gruvbox, Matrix, Solarized, TokyoNight, …). Configurable font size.

## Auto-updates

Background checks against GitHub Releases, SHA-256 verified. macOS replaces
the app bundle and relaunches. On Linux, a CLI installed from the desktop app
is refreshed on the next launch after an update.

## Demos not inlined here

These recordings are too large for the docs site; they stay in
[`assets/animations/`](https://github.com/boersmamarcel/chatty2/tree/main/assets/animations)
and on the [marketing repo](https://github.com/boersmamarcel/chatty):

| File | Shows |
|------|-------|
| `hero_high_quality.gif` | App overview (also on the repo README) |
| `add_provider_and_model.gif` | First-run provider + model setup |
| `file_add_edit_delete.gif` | File tools |
| `shell_command.gif` | Sandboxed shell |
| `mcp_add_edit_delete2.gif` | MCP server management |
