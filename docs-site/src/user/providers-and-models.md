# Providers & models

Connect an LLM backend and choose which models appear in the picker.

## Add a provider

**Settings → Providers → Add Provider.** Supported types include OpenRouter,
Azure OpenAI, Ollama, Anthropic, OpenAI, Gemini, and Mistral.

Ollama connects to a local instance automatically — no API key.

## Add a model

**Settings → Models → Add Model**: pick the provider and enter a model ID
(for example `claude-sonnet-4-20250514`, `gpt-4o`, `qwen2.5:0.5b`).

Chatty stores per-model capabilities so the UI can show the right attachment
buttons and temperature control:

| Capability | What it controls |
|------------|------------------|
| Images | Vision attachments in the chat input |
| PDF | PDF attachments |
| Temperature | Temperature slider (hidden for reasoning models that reject it) |

Ollama capabilities are detected per model via the local `/api/show` API.

| Provider | Images | PDF | Temperature | Notes |
|----------|:------:|:---:|:-----------:|-------|
| OpenRouter | Per-model | Per-model | Yes | Routes to Anthropic, OpenAI, Google, Mistral, and others |
| Azure OpenAI | Yes | Lossy | Yes | API key or Entra ID |
| Ollama | Per-model | Per-model | — | Detected locally |

The full provider matrix (auth, TUI flags, defaults) is in the
[developer reference](../dev/reference/provider-matrix.md).

## Next

- [Getting started](./getting-started.md)
- [Agents](./agents.md)
