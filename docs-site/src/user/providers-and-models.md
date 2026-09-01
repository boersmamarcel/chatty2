# Providers & models

**When to read this:** Connect an LLM backend and pick models.

Developer lookup table: [Provider matrix](../dev/reference/provider-matrix.md).

## Add a provider

**Settings → Providers → Add Provider.** Supported types include OpenRouter,
Azure OpenAI, Ollama, Anthropic, OpenAI, Gemini, and Mistral.

- **OpenRouter** — one key, many upstream models (Claude, GPT, Gemini, Mistral, …)
- **Azure OpenAI** — API key or Entra ID
- **Ollama** — local instance, no API key; Chatty probes `localhost:11434`

## Add a model

**Settings → Models → Add Model.** Pick the provider, then enter a model ID
such as `claude-sonnet-4-20250514`, `gpt-4o`, or `qwen2.5:0.5b`.

Chatty records three capabilities per model and uses them to show or hide
attachment buttons and the temperature control:

| Capability | What it controls |
|------------|------------------|
| Images | Vision attachments in chat |
| PDF | PDF attachments |
| Temperature | Temperature slider (off for some reasoning models) |

Ollama capabilities are detected per model (`/api/show`) and stored with the
model config so they survive restarts.

### Defaults by provider

| Provider | Images | PDF | Temperature | Notes |
|----------|:------:|:---:|:-----------:|-------|
| OpenRouter | Per-model | Per-model | Yes | Routes to many upstreams |
| Azure OpenAI | Yes | Lossy | Yes | API key or Entra ID |
| Ollama | Per-model | Per-model | — | Fully local |

## Next

- [Getting started](./getting-started.md)
- [Features](./features.md)
