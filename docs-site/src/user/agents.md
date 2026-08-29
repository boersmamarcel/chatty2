# Agents

**When to read this:** Understand how the agent loop works and what agents can do autonomously.

## How the agent loop works

Each message builds a full agent with your configured tools and MCP servers, then runs a streaming multi-turn loop:

```
You send a message
       │
       ▼
  Agent reasons → calls a tool
       │
       ▼
  Tool executes (with approval if required)
       │
       ▼
  Agent receives result → reasons again
       │
       ▼
  ...repeats up to turn limit (default 10)...
       │
       ▼
  Final answer streamed to you
```

Tool calls, inputs, outputs, and reasoning appear as collapsible trace blocks alongside the response.

For multi-step tasks, the agent creates a **structured plan** (goal + ordered todos). A collapsible **Agent plan** panel shows each step's status and progress before the agent marks steps done and verifies completion.

## What agents can do

With tools enabled, an agent can:

- **Explore and edit code** — read files, navigate directories, apply diffs, rename and delete
- **Execute shell commands** — builds, tests, git, scripts inside a sandbox
- **Write and run code** — Python, JavaScript, TypeScript, Rust, or Bash (MontySandbox fast path; Docker fallback for packages)
- **Query data** — SQL over Parquet/CSV/JSON via DuckDB; read/write Excel, Word, PowerPoint
- **Browse the web** — Tavily, Brave, or DuckDuckGo lite fallback; fetch and parse URLs
- **Generate outputs** — charts, Typst PDFs, diagrams, inline images
- **Remember and learn** — persistent memory across conversations ([memory & skills](./memory-and-skills.md))
- **Delegate** — spawn [sub-agents](./sub-agents.md) for parallel work

## Slash commands

Type `/` in the chat input:

| Command | What it does |
|---------|--------------|
| `/agent <prompt>` | Spawn a headless `chatty-tui` sub-agent |
| `/compact` | Summarize older history to free context |
| `/context` | Token usage, context fill, working directory |
| `/add-dir <path>` | Expand workspace to another directory |
| `/cwd` / `/cd <path>` | Show or change working directory |
| `/new` / `/clear` | Fresh conversation |
| `/copy` | Copy latest response to clipboard |
| `[skill name]` | Invoke a saved skill from the picker |

Full list: [slash commands reference](../dev/reference/slash-commands.md).

## Extended thinking

Models with chain-of-thought reasoning (e.g. Claude extended thinking) render `<thinking>` blocks as collapsible sections.

## Context window management

- **Fill bar** — segmented footer showing context by component (preamble, tools, history, latest message)
- **Token popover** — hover for per-segment estimates and provider token counts
- **`/compact`** — compress older messages when the window fills
- **Max Context Window** — set per model in Settings → Models → Advanced to enable the fill bar

## Next

- [Agentic tools](./agentic-tools.md)
- [Memory & skills](./memory-and-skills.md)
- [Sub-agents](./sub-agents.md)
