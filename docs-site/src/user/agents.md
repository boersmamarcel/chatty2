# Agents

**When to read this:** How the agent loop works and what it can do once tools
are enabled.

## How the loop works

Each message builds an agent with your configured tools and MCP servers, then
starts a streaming multi-turn loop:

```
You send a message
       │
       ▼
  Agent reasons → decides to call a tool
       │
       ▼
  Tool executes (with your approval if required)
       │
       ▼
  Agent receives result → reasons again
       │
       ▼
  ...repeats up to the turn limit (default 10; Settings → Code Execution)...
       │
       ▼
  Agent produces a final answer → streamed to you
```

Tool calls, inputs, outputs, and reasoning render as collapsible trace blocks
beside the response.

For multi-step work the agent writes a **structured plan** first: a goal and an
ordered todo list. A collapsible **Agent plan** panel shows each step (pending,
in-progress, done, blocked) and a progress counter. The agent marks steps as it
goes, then runs a verification step before the final reply.

## What agents can do

With tools on ([Agentic tools](./agentic-tools.md)), an agent can:

- **Explore and edit a codebase** — read files, list directories, apply diffs, rename, delete
- **Run shell commands** — builds, tests, git, scripts inside the sandbox
- **Write and run code** — Python, JavaScript, TypeScript, Rust, or Bash
  (MontySandbox fast path; Docker fallback for packages and other languages)
- **Query data** — SQL over Parquet, CSV, and JSON via DuckDB; Excel, Word, PowerPoint
- **Browse the web** — Tavily or Brave if you set a key; DuckDuckGo lite otherwise; `fetch` any URL
- **Produce artifacts** — charts, Typst PDFs, diagrams, inline images
- **Inspect its own tools** — list built-in tools and configured MCP servers
- **Remember** — store facts and procedures across conversations
  ([Memory & skills](./memory-and-skills.md))
- **Delegate** — spawn [sub-agents](./sub-agents.md) for parallel subtasks

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
| `/copy` | Copy latest response to the clipboard |
| `[skill name]` | Invoke a saved skill from the picker |

Full list: [slash commands reference](../dev/reference/slash-commands.md).

## Extended thinking

Models that emit chain-of-thought (e.g. Claude extended thinking) render
`<thinking>`, `<think>`, and `<thought>` as collapsible sections so the
reasoning is inspectable without filling the transcript.

## Context window

Long runs fill the window quickly. Chatty exposes:

- **Fill bar** — footer segments for preamble, tool definitions, history, latest message (green / amber / red)
- **Token popover** — hover for per-segment estimates and provider input/output counts
- **`/compact`** — summarize older messages so the run can continue
- **Max Context Window** — set on the model under Settings → Models → Advanced to turn the bar on

## Next

- [Agentic tools](./agentic-tools.md)
- [Memory & skills](./memory-and-skills.md)
- [Sub-agents](./sub-agents.md)
