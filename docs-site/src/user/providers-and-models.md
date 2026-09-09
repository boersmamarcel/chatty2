# Providers & models

**When to read this:** You want to connect an LLM provider, add models to the roster, pick a default, or work out why a model is missing.

Everything lives on one page: **Settings → Models & Providers**. The roster lists every model as a row — favourite star, provider, context window, input price per million tokens and temperature — with a status chip per provider across the top. Changes save automatically.

## Connect a provider

Click **Manage keys**. One sheet holds all three providers; each row shows its status and has **Save** and **Test** buttons. Keys are stored with your app settings, never in your project files. Removing a key hides that provider's models but keeps their settings.

### OpenRouter

One key gives access to many upstream models (Claude, GPT, Gemini, Mistral and more). Paste the key (it starts with `sk-or-`), **Save**, then **Test** — a good key shows **Key verified** and the chip reads *Connected · N models*. Chatty also pulls the OpenRouter catalogue, including pricing, so the roster's price column fills itself.

### Ollama

No key needed — a local instance is detected automatically. The URL field defaults to `http://localhost:11434`; change it and **Save** if Ollama runs elsewhere (another machine, a different port). **Test** shows *Running · N models*.

Ollama models are added for you: Chatty discovers what is installed shortly after launch and keeps the roster in step, removing models you have deleted from Ollama and detecting per model whether it accepts images.

> [!NOTE]
> **A pulled model does not show up?** Discovery runs once, shortly after Chatty starts. If you `ollama pull` a model while Chatty is open, restart Chatty — **Test** confirms Ollama is reachable but does not add models. If **Test** fails, make sure Ollama is running (`ollama list` in a terminal) and that the URL matches where it listens. Models you added by hand under **Enter an identifier manually** are left alone by discovery.

### Azure OpenAI

Azure serves your own deployments rather than a public catalogue, so it needs three things:

1. **API key**, or switch on **Use Entra ID instead of a key** to sign in with your Azure account.
2. **Endpoint URL** — your resource address, e.g. `https://my-resource.openai.azure.com`.
3. **Deployment name** — the deployment you created in Azure, e.g. `gpt-5-chat`.

Press **Connect & fetch models**. Each further deployment is added by name in the **Add model** sheet. The API version can be adjusted per model under **Edit… → Advanced**.

## Add a model

Click **Add model**. The sheet opens on the provider's catalogue: search it, tick as many models as you like, and add them in one go — no identifiers to type. Models already in your roster show as added. If the OpenRouter catalogue is empty, add a key in **Manage keys** first and reopen the sheet.

For anything the catalogue does not list, expand **Enter an identifier manually** and give it a display name and the provider's model identifier (for example `qwen2.5:0.5b`).

## Favourites, default and per-model settings

- Click a row's star to pin the model to the top; the **Favourites** and **Local** chips filter the roster.
- A row's **⋯** menu offers **Set as default** (the model new conversations start with), **Edit…** and **Remove**.
- **Edit…** has a **Basic** tab (name, identifier, temperature, system prompt) and an **Advanced** tab: max tokens, **Max Context Window** (turns on the context fill bar in the chat footer), top-p, cost per million input and output tokens, and the Azure API version.

Favourites and the default survive catalogue syncs and restarts.

## Capabilities

Chatty records three capabilities per model and uses them to show or hide the attachment buttons and the temperature control:

| Capability | What it controls |
|------------|------------------|
| Images | Image attachments in chat |
| PDF | PDF attachments |
| Temperature | The temperature slider (off for some reasoning models) |

Starting values by provider: OpenRouter models accept images and PDFs; Azure OpenAI models accept images but not PDFs; Ollama models are detected one by one, so a vision model gets the image button and a text-only model does not.

## Next

- [Getting started](./getting-started.md)
- [Chatting](./chatting.md)
- [Advanced](./advanced.md)
