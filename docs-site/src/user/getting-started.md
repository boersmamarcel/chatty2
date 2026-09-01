# Getting started

Set up Chatty and send your first message.

For a product overview, see [Why Chatty?](./overview.md). Demos and marketing
copy live on the [marketing site](https://github.com/boersmamarcel/chatty).

## 1. Download

Grab the latest release from
[GitHub Releases](https://github.com/boersmamarcel/chatty2/releases):

| Platform | Format |
|----------|--------|
| macOS (Intel & Apple Silicon) | `.dmg` installer |
| Linux (x86_64) | `.tar.gz` archive |
| Windows (x86_64) | `.exe` installer |

## 2. Add a provider

1. Click the **gear icon** in the title bar to open Settings.
2. Open the **Providers** tab → **Add Provider**.
3. Choose a provider (OpenRouter, Ollama, Azure OpenAI, …).
4. Paste an API key. Ollama talks to your local instance and does not need a
   key.

## 3. Add a model

1. **Settings → Models → Add Model**
2. Pick the provider and enter a model ID (for example `gpt-4o` or
   `claude-sonnet-4-20250514`).
3. Chatty detects vision and PDF support from the model — no extra flags.

See [Providers & models](./providers-and-models.md) for the capability table.

![Adding a provider and model](./img/add_provider_and_model.gif)

## 4. Start chatting

Close Settings and send a message. A new conversation shows a start screen
with the capabilities that are active: skills, MCP servers, agents, file
access, web tools, memory, and workspace.

- Type `/` for slash commands (`/clear`, `/compact`, `/context`, `/agent`, …).
  Skills saved in `.claude/skills/` or the global skills directory appear with
  a `[skill]` badge.
- Type `@` to insert a file from the working directory. Hidden files and
  common build folders (`.git`, `node_modules`, `target`) are omitted.
- Switch models from the selector at the bottom of the chat.

## 5. Enable agentic tools

Tools are off by default. Enable them in **Settings → Code Execution**:

1. Set a **workspace directory** (absolute path). The agent can only touch
   files inside it.
2. Turn **code execution** on.
3. Pick an **approval mode**: ask every time (recommended at first),
   auto-approve, or deny all.

Optionally set a **per-chat working directory** with the folder icon in the
chat input. The override is stored with the conversation.

Fast Python (MontySandbox) and Docker fallback are covered in
[Security & sandboxing](./security.md). The tool list is in
[Agentic tools](./agentic-tools.md).

## Desktop vs terminal

| App | Use when |
|-----|----------|
| `chatty` (GPUI) | Daily interactive work, settings, attachments |
| `chatty-tui` | Terminal, scripts, headless sub-agents |

Install `chatty-tui` from the desktop app (**Install CLI**) or from the same
release archive. Full guide: [Terminal interface](./terminal.md).

## Next

- [Why Chatty?](./overview.md)
- [Agents](./agents.md)
- [Providers & models](./providers-and-models.md)
- [Agentic tools](./agentic-tools.md)
