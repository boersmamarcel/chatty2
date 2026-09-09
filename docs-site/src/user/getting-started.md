# Getting started

**When to read this:** You are installing Chatty for the first time and want to send your first message.

Chatty is a desktop and terminal AI agent that runs on your own machine: your keys, conversations and files stay local, and the model only reaches what you switch on. Why it exists: [the landing page](../index.md).

![Chatty overview](../assets/animations/hero.gif)

## 1. Install

Download the latest release from [GitHub Releases](https://github.com/boersmamarcel/chatty2/releases):

| Platform | File |
|----------|------|
| macOS (Intel and Apple Silicon) | `.dmg` |
| Linux (x86_64) | `.AppImage` |
| Windows (x86_64) | `.exe` installer |

Chatty checks for new releases in the background and offers them in the status footer. Details: [Advanced](./advanced.md).

## 2. Connect a provider and add a model

1. Click the gear icon in the title bar to open Settings, then **Models & Providers**.
2. **Manage keys** — paste an OpenRouter key, point at a local Ollama, or connect Azure OpenAI. Press **Test** to check it.
3. **Add model** — search the provider's catalogue, tick the models you want, then **Add**.

Ollama models appear on their own once Ollama is running. Azure fields, the roster, favourites and troubleshooting: [Providers & models](./providers-and-models.md).

## 3. Send a message

Close Settings and type. A new conversation opens on a start screen that shows what is switched on — modules, MCP servers, agents, file access, memory and whether a workspace is set — so you know what the agent can reach before you ask. Switch models with the selector at the bottom of the chat.

- Type `/` for the command picker (`↑/↓`, `Enter`).
- Type `@` to mention a file from the working directory.

Rendering, attachments, artifacts, cost tracking and search: [Chatting](./chatting.md).

## 4. Turn on tools (optional)

Tools are off by default, so at this point Chatty is a chat window. To let the agent read and edit files, run commands and use extensions, open Settings → **Code Execution**, set a **Workspace Directory**, switch on **Enable Code Execution** and choose an approval mode. Walkthrough: [Agents & tools](./agents-and-tools.md). What each approval mode does and how the sandbox works: [Security & sandboxing](./security.md).

> [!TIP]
> Web access is a separate switch (Settings → **Internet**) and is on by default, so the agent can fetch pages and search the web even before you enable code execution.

## Desktop or terminal?

| App | Use when |
|-----|----------|
| Chatty (desktop) | Daily work, settings, attachments, artifacts |
| `chatty-tui` | Terminal sessions, scripts and pipelines, headless sub-agents |

Both share the same providers and models. Install the terminal app from the desktop app: [Terminal interface](./terminal.md).

## Next

- [Providers & models](./providers-and-models.md)
- [Chatting](./chatting.md)
- [Agents & tools](./agents-and-tools.md)
