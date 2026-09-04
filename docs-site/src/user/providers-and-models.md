# Providers & models

**When to read this:** Connect an LLM backend and pick models.

Developer lookup table: [Provider matrix](../dev/reference/provider-matrix.md).

Models and providers share one page: **Settings → Models & Providers**. It
lists every model as a row — favourite, default, provider, context window,
input price and temperature visible without opening anything — with a status
chip per provider across the top.

## Connect a provider

**Settings → Models & Providers → Manage keys.** One sheet holds all three
providers; each row shows its status and has a Test button.

- **OpenRouter** — one key, many upstream models (Claude, GPT, Gemini, Mistral, …)
- **Azure OpenAI** — API key or Entra ID, plus endpoint and deployment name
- **Ollama** — local instance, no API key; Chatty probes `localhost:11434`

## Add a model

**Settings → Models & Providers → Add model.** The sheet opens on the
provider's catalogue: search it, tick as many models as you want, add them in
one go — no identifiers to type. Models already in your roster show as added.
For anything the catalogue doesn't list, expand **Enter an identifier
manually** and type one such as `qwen2.5:0.5b` directly.

Ollama models are the exception — they appear on their own as you pull them.

## Favourites and the default model

Click a row's star to pin a model to the top of the roster. A row's ⋯ menu
sets the default — the model new conversations start with. Both survive
provider syncs and restarts.

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
