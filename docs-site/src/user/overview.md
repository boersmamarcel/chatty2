# Why Chatty?

**When to read this:** You want a product overview before diving into setup.

For downloads and first-run steps, see [Getting started](./getting-started.md).

## Designed for agentic work

Chatty is built for multi-turn agents, not just chat. Your LLM can chain dozens of tool calls — read files, run shell commands, query databases, browse the web, write and execute code, generate charts and documents, spawn sub-agents — and return a complete answer. The app runs locally under your control; network access depends on the provider and tools you enable.

## Your keys, your data

No middleman, no subscriptions. Chatty talks directly to OpenRouter, Azure OpenAI, your local Ollama instance, and any MCP/A2A services you configure. Conversations and settings are stored locally; prompts, attachments, and tool results may still be sent to the remote providers or services you choose.

## Native Rust performance

Built on [GPUI](https://crates.io/crates/gpui), the GPU-accelerated framework behind the Zed editor — not an Electron wrapper. Instant startup, smooth scrolling, minimal memory footprint.

## One app, every model

Access hundreds of models through OpenRouter, Azure OpenAI, or Ollama from a single window. Switch models mid-conversation and use the right model for each task.

## Real tool use, properly sandboxed

Filesystem access, a bash shell, and MCP servers run within a workspace sandbox. On Linux, shell commands use [bubblewrap](https://github.com/containers/bubblewrap) namespace isolation. On macOS, they use `sandbox-exec` with policy profiles that block `.ssh`, `.aws`, and other sensitive directories. You choose the approval mode: ask every time, auto-approve, or deny all.

## Privacy-aware by default

Chatty does not run its own cloud relay or product telemetry. Conversations are stored in a local SQLite database. For a fully local setup, use Ollama and avoid networked tools; otherwise, data goes directly to the providers and services you enable.

## Marketing site

Product demos and GIFs remain on the **[marketing site](https://github.com/boersmamarcel/chatty)** until the README split review (DOC-15) is complete.
