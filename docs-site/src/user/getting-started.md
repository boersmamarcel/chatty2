# Getting started

**When to read this:** You are an end user setting up Chatty for the first time.

For product overview, see [Why Chatty?](./overview.md). For demos and marketing content, see the **[marketing site](https://github.com/boersmamarcel/chatty)**.

## 1. Download

Grab the latest release from [GitHub Releases](https://github.com/boersmamarcel/chatty2/releases):

| Platform | Format |
|----------|--------|
| macOS (Intel & Apple Silicon) | `.dmg` installer |
| Linux (x86_64) | `.tar.gz` archive |
| Windows (x86_64) | `.exe` installer |

## 2. Add a provider

1. Click the **gear icon** in the title bar → **Settings**
2. Go to **Providers** → **Add Provider** (OpenRouter, Ollama, Azure OpenAI, etc.)
3. Paste your API key (Ollama connects locally — no key needed)

## 3. Add a model

1. **Settings → Models → Add Model**
2. Pick a provider and enter a model ID (e.g. `gpt-4o`, `claude-sonnet-4-20250514`)
3. Chatty auto-detects vision and PDF support

See [Providers & models](./providers-and-models.md) for capability details.

## 4. Start chatting

Close Settings and send your first message. The start screen shows active capabilities — skills, MCP servers, agents, file access, web tools, memory, workspace status.

- Type `/` for slash commands (`/clear`, `/compact`, `/context`, `/agent`, …)
- Type `@` for file picker in the current working directory
- Switch models with the selector at the bottom of the chat

## 5. Enable agentic tools

Off by default — enable in **Settings → Code Execution**:

1. Set a **workspace directory** (absolute path)
2. Toggle **code execution** on
3. Choose **approval mode**: ask every time / auto-approve / deny all

Optional: set a **per-chat working directory** via the folder icon in the chat input.

For MontySandbox fast Python and Docker fallback, see [Security & sandboxing](./security.md).

## Desktop vs terminal

| App | Use when |
|-----|----------|
| `chatty` (GPUI) | Daily interactive work, settings UI, attachments |
| `chatty-tui` | Terminal, scripting, headless sub-agents |

```bash
cargo run -p chatty-gpui    # desktop
cargo run -p chatty-tui     # terminal
```

Full terminal guide: [Terminal interface](./terminal.md).

## Next

- [Why Chatty?](./overview.md)
- [Agents](./agents.md)
- [Providers & models](./providers-and-models.md)
- [Agentic tools](./agentic-tools.md)
