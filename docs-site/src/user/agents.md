# Agents

How the agent loop works and what it can do once tools are enabled.

## How the loop works

Each message builds an agent with your configured tools and MCP servers, then
runs a streaming multi-turn loop:

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
  ...repeats up to the turn limit (default 10)...
       │
       ▼
  Final answer streamed to you
```

Tool calls, inputs, outputs, and reasoning appear as collapsible trace blocks
next to the reply.

For multi-step work the agent writes a **structured plan** (goal + ordered
todos). A collapsible **Agent plan** panel shows each step's status. The agent
marks steps as it goes and calls a verification step before the final reply.

Turn limit and code-execution settings live in **Settings → Code Execution**.

## What agents can do

With tools enabled, an agent can:

- **Explore and edit code** — read files, navigate directories, apply diffs,
  rename and delete
- **Run shell commands** — builds, tests, git, scripts inside a sandbox
- **Write and run code** — Python, JavaScript, TypeScript, Rust, or Bash
  (MontySandbox fast path; Docker fallback for packages)
- **Query data** — SQL over Parquet/CSV/JSON via DuckDB; read/write Excel,
  Word, PowerPoint
- **Browse the web** — Tavily, Brave, or DuckDuckGo lite fallback; fetch URLs
- **Produce outputs** — charts, Typst PDFs, diagrams, inline images
- **Remember** — persistent memory across conversations
  ([memory & skills](./memory-and-skills.md))
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

Models with chain-of-thought reasoning (for example Claude extended thinking)
render `<thinking>` / `<think>` / `<thought>` blocks as collapsible sections.

## Context window

- **Fill bar** — footer segments for preamble, tools, history, and the latest
  message (green / amber / red)
- **Token popover** — hover for per-segment estimates and provider counts
- **`/compact`** — compress older messages when the window fills
- **Max Context Window** — set per model in Settings → Models → Advanced to
  enable the fill bar

Token UI is also shown under [Features](./features.md).

## Next

- [Agentic tools](./agentic-tools.md)
- [Memory & skills](./memory-and-skills.md)
- [Sub-agents](./sub-agents.md)
