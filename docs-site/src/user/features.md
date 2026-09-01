# Features

Product capabilities beyond the agent loop. Setup is in
[Getting started](./getting-started.md).

## Multi-provider support

Connect more than one LLM provider and switch models mid-conversation.
Per-model vision, PDF, and temperature support is stored with the model.
See [Providers & models](./providers-and-models.md).

## Rich rendering

- **Markdown** with standard formatting
- **Syntax-highlighted code** (30+ languages via tree-sitter) with one-click copy
- **LaTeX math** — inline `$...$` and block `$$...$$`, compiled to SVG via Typst
- **Mermaid diagrams** — 23 diagram types, theme-aware, copy as PNG
- **Image and PDF** previews inline in chat

![LaTeX math rendering](./img/advanced_math_rendering.gif)

![Mermaid diagram rendering](./img/mermaid.gif)

![Syntax-highlighted code](./img/codehighlighting.gif)

## Tool call traces

Each tool call is a collapsible block: name, arguments, output, duration, and
status. `apply_diff` calls show a visual diff (additions green, deletions red).
Long unchanged spans collapse; large diffs offer "Show N more lines".

## Conversations & cost tracking

- Conversations persist in a local SQLite database (no Chatty-hosted sync)
- Auto-generated titles; search from the title-bar search icon
- Export a conversation to Markdown from the sidebar `…` menu
- Per-conversation cost in the sidebar; per-message token counts
- Regenerating a reply keeps the original, which can become a DPO pair

![Token and cost tracking](./img/advanced_token_tracking.gif)

## Training data export

Export agent conversations for fine-tuning pipelines.

### ATIF (Agent Trajectory Interchange Format)

Structured JSON: messages, tool calls, reasoning, timestamps, token metrics,
feedback, and regeneration pairs. Compatible with
[Harbor Framework](https://harborframework.com/docs/agents/trajectory-format)
workflows.

### JSONL

- **SFT** — ChatML for OpenAI, Anthropic, Together AI, and similar APIs
- **DPO** — preference pairs from regenerated responses
- Re-exporting a conversation replaces the previous entry

| Platform | Export path |
|----------|-------------|
| macOS | `~/Library/Application Support/chatty/exports/` |
| Linux | `~/.config/chatty/exports/` |
| Windows | `%APPDATA%\chatty\exports\` |

Enable auto-export in **Settings → Training Data**.

## Environment secrets

**Settings → Secrets** holds key-value pairs injected into every agent shell
session. The agent sees variable *names* (so it can write
`os.environ["API_KEY"]`) but never the values. Secrets are masked in tool
output.

## Themes & UI

Twenty-plus themes with light and dark variants. Font size is configurable.

## Auto-updates

Background checks against GitHub releases, verified with SHA-256. On macOS the
app bundle is replaced and relaunched. On Linux, a `chatty-tui` installed from
the desktop app refreshes on the next launch.

## Next

- [Agents](./agents.md)
- [Agentic tools](./agentic-tools.md)
- [Terminal interface](./terminal.md)
