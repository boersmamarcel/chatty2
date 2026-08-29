# Getting started

**When to read this:** You are an end user setting up Chatty for the first time.

For product overview and downloads, see the **[marketing site](https://github.com/boersmamarcel/chatty)**.

## Quick steps

1. **Download** the latest release from [GitHub Releases](https://github.com/boersmamarcel/chatty2/releases)
2. **Add a provider** in Settings → Providers (OpenRouter, Ollama, Azure, etc.)
3. **Add a model** in Settings → Models
4. **Enable agentic tools** in Settings → Code Execution (workspace path + approval mode)

## Desktop vs terminal

| App | Use when |
|-----|----------|
| `chatty` (GPUI) | Daily interactive work, settings UI, attachments |
| `chatty-tui` | Terminal, scripting, headless sub-agents |

```bash
cargo run -p chatty-gpui    # desktop
cargo run -p chatty-tui     # terminal
```

## Next

- [Providers & models](./providers-and-models.md)
- [Agentic tools](./agentic-tools.md)
