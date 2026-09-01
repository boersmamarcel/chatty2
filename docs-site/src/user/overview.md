# Why Chatty?

A short product overview. Setup steps are in
[Getting started](./getting-started.md).

![Chatty in action](./img/hero_high_quality.gif)

## Designed for agentic work

Chatty is built for multi-turn agents, not one-shot chat. The model can chain
tool calls — read files, run shell commands, query data, browse the web, write
and run code, produce charts and documents, spawn sub-agents — and return a
finished answer. The app runs on your machine; what leaves the machine depends
on the providers and tools you enable.

## Your keys, your data

Chatty talks directly to OpenRouter, Azure OpenAI, a local Ollama instance, and
the MCP/A2A services you configure. There is no Chatty-hosted relay or
subscription. Conversations and settings stay in local files; prompts,
attachments, and tool results still go to the remote services you choose.

## Native Rust UI

The desktop app uses [GPUI](https://crates.io/crates/gpui), the GPU-accelerated
framework behind Zed — not an Electron wrapper.

## One window, many models

Switch between models mid-conversation. OpenRouter routes to Anthropic,
OpenAI, Google, Mistral, and others; Azure OpenAI and Ollama are first-class
too. See [Providers & models](./providers-and-models.md).

## Sandboxed tools

Filesystem access, a shell, and MCP servers stay inside the workspace you set.
Linux uses [bubblewrap](https://github.com/containers/bubblewrap); macOS uses
`sandbox-exec` profiles that block `.ssh`, `.aws`, and similar paths. Approval
mode is yours: ask every time, auto-approve, or deny all. Details:
[Security & sandboxing](./security.md).

## Privacy defaults

Chatty does not run product telemetry or a hosted conversation store.
Conversations live in a local SQLite database. A fully local setup means
Ollama plus no networked tools; otherwise data goes to the providers and
services you turn on.

## Marketing

Product landing page and extra demos:
[github.com/boersmamarcel/chatty](https://github.com/boersmamarcel/chatty).
