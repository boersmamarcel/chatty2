# Why Chatty?

**When to read this:** Product overview before setup. How-to steps live in
[Getting started](./getting-started.md).

Chatty is a local desktop and terminal agent. It talks to the LLM providers
you configure, stores conversations on disk, and optionally gives the model
sandboxed tools. Marketing copy and extra demos stay on
[boersmamarcel/chatty](https://github.com/boersmamarcel/chatty).

## Agentic loop, not just chat

Each send builds an agent with your tools and MCP servers, then runs a
multi-turn reason → act → observe loop until the task is done or the turn
limit is hit. The model can read and edit files, run shell commands, query
data, browse the web, execute code, produce charts and documents, and spawn
[sub-agents](./sub-agents.md). Network use depends on the providers and tools
you enable. See [Agents](./agents.md).

## Keys and data stay on your machine

Chatty has no hosted relay and no product telemetry. API keys live in local
config. Conversations sit in a local SQLite database. Prompts, attachments,
and tool results still go to whatever remote providers, MCP/A2A services, or
websites you choose to call. A fully local setup is Ollama plus networked
tools left off.

## Native UI

The desktop app uses [GPUI](https://crates.io/crates/gpui) (the GPU UI
framework behind Zed). `chatty-tui` uses Ratatui. Neither is an Electron
wrapper.

## One window, many models

OpenRouter (Anthropic, OpenAI, Google, Mistral, and others), Azure OpenAI, and
local Ollama share one model picker. Switch mid-conversation. Per-model vision
/ PDF / temperature flags are documented in
[Providers & models](./providers-and-models.md).

## Sandboxed tools

Filesystem and shell tools are scoped to a workspace directory you set.
Linux uses [bubblewrap](https://github.com/containers/bubblewrap) namespaces;
macOS uses `sandbox-exec` profiles that block `.ssh`, `.aws`, and similar
paths. Approval mode is ask / auto / deny. Details:
[Security & sandboxing](./security.md).

## Next

- [Getting started](./getting-started.md)
- [Features](./features.md)
- [Agentic tools](./agentic-tools.md)
