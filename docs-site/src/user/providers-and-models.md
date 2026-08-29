# Providers & models

**When to read this:** Configure LLM backends and model capabilities.

## Providers

Open **Settings → Providers → Add Provider**. Supported types include Anthropic,
OpenAI, Gemini, Ollama, Mistral, Azure OpenAI, and OpenRouter.

Ollama connects to your local instance automatically — no API key required.

## Models

**Settings → Models → Add Model**: pick a provider and enter a model ID
(e.g. `claude-sonnet-4-20250514`, `gpt-4o`, `qwen2.5:0.5b`).

Chatty stores per-model capabilities in `ModelConfig`:

| Field | Meaning |
|-------|---------|
| `supports_images` | Vision attachments |
| `supports_pdf` | PDF attachments |
| `supports_temperature` | Temperature slider (off for reasoning models) |

Defaults come from `ProviderType::default_capabilities()`; Ollama models are
detected per-model via `/api/show`.

Developer reference: [Provider matrix](../dev/reference/env-vars.md) ·
[agent_factory](https://github.com/boersmamarcel/chatty2/tree/main/crates/chatty-core/src/factories/agent_factory).
